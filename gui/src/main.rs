//! GUI per scaricare i file dei gruppi Telegram: login, scelta del gruppo, scelta dei file.
#![cfg_attr(windows, windows_subsystem = "windows")] // su Windows niente finestra di console
use std::fmt::Display;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use grammers_client::client::PasswordToken;
use grammers_client::media::Media;
use grammers_client::peer::Peer;
use grammers_client::{Client, InvocationError, SenderPool, SignInError, tl};
use grammers_session::Session;
use grammers_session::storages::SqliteSession;
use grammers_session::types::PeerRef;
use rune::color::hex;
use rune::{AppSpec, Color, Cursor, Cx, El, Family, FileDialogOptions, GrowthSpec, ScrollSpec, TextEdit, TextStyle, Tx, col, el, row, spacer, text, text_input};
use rune_motion::Cycle;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Foto,
    Video,
    Audio,
    Documento,
}

struct FileItem {
    name: String,
    kind: Kind,
    size: usize,
    media: Option<Media>, // None solo nelle scene di esempio (--snap)
    selected: bool,
    done: bool, // già sul disco
}

#[derive(Clone, PartialEq)]
enum Screen {
    Setup,
    Phone,
    Code,
    Password(String),
    Qr(String), // link tg://login da mostrare come QR code
    Main,
}

struct State {
    screen: Screen,
    busy: bool,
    error: Option<String>,
    status: String,
    client: Option<Client>,
    session: Option<Arc<SqliteSession>>,
    api_id: i32,
    login: Option<(String, String)>, // numero e phone_code_hash del codice in attesa
    code_info: String,                // dove Telegram dice di aver mandato il codice
    password_token: Option<PasswordToken>,
    login_update: Arc<tokio::sync::Notify>, // Telegram ci avvisa quando il QR è stato scansionato
    groups: Vec<(String, PeerRef)>,
    current: Option<usize>,
    scan_gen: u64, // cambia quando si sceglie un altro gruppo: la scansione vecchia si ferma
    scanning: bool,
    files: Vec<FileItem>,
    progress: Option<(usize, usize)>,
}

type Shared = Arc<Mutex<State>>;

fn err(e: impl Display) -> String {
    e.to_string()
}

fn data_dir() -> PathBuf {
    let d = std::env::home_dir().unwrap_or_default().join(".telegram-downloader");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Nome sul disco: `<id messaggio>_<nome>`. Il nome arriva da Telegram, quindi
/// teniamo solo l'ultima parte del percorso per non uscire dalla cartella.
/// Toglie i caratteri che Windows non accetta nei nomi di file e cartelle.
fn safe(name: &str) -> String {
    name.chars().map(|c| if c.is_control() || r#"/\:*?"<>|"#.contains(c) { '_' } else { c }).collect()
}

fn file_name(msg_id: i32, name: Option<&str>, mime: &str) -> String {
    // safe() toglie / \ e : (niente percorsi né unità disco), file_name() scarta "." e ".."
    let name = name.map(safe);
    match name.as_deref().and_then(|n| Path::new(n).file_name()).and_then(|n| n.to_str()) {
        Some(base) => format!("{msg_id}_{base}"),
        None => {
            let ext = mime.rsplit('/').next().filter(|e| !e.is_empty()).unwrap_or("bin");
            format!("{msg_id}_file.{ext}")
        }
    }
}

fn to_item(msg_id: i32, media: Media) -> Option<FileItem> {
    let (name, size, kind) = match &media {
        Media::Photo(p) => (format!("{msg_id}.jpg"), p.size()?, Kind::Foto),
        Media::Document(d) => {
            let mime = d.mime_type().unwrap_or("");
            let kind = match mime.split('/').next() {
                Some("image") => Kind::Foto,
                Some("video") => Kind::Video,
                Some("audio") => Kind::Audio,
                _ => Kind::Documento,
            };
            (file_name(msg_id, d.name(), mime), d.size()?, kind)
        }
        _ => return None, // sticker, sondaggi, posizioni...
    };
    Some(FileItem { name, kind, size, media: Some(media), selected: false, done: false })
}

// ---------- operazioni Telegram (girano su tokio) ----------

async fn connect(st: Shared, api_id: i32, api_hash: String) -> Result<(), String> {
    let session = Arc::new(SqliteSession::open(data_dir().join("session")).await.map_err(err)?);
    let SenderPool { runner, handle, mut updates } = SenderPool::new(session.clone(), api_id);
    let client = Client::new(handle);
    tokio::spawn(runner.run());
    let notify = st.lock().unwrap().login_update.clone();
    tokio::spawn(async move {
        while updates.recv().await.is_some() {
            notify.notify_one();
        }
    });
    let authorized = client.is_authorized().await.map_err(err)?;
    let _ = std::fs::write(data_dir().join("config"), format!("{api_id}\n{api_hash}"));
    {
        let mut s = st.lock().unwrap();
        s.client = Some(client.clone());
        s.session = Some(session);
        s.api_id = api_id;
    }
    if authorized {
        load_groups(st, client).await
    } else {
        let mut s = st.lock().unwrap();
        s.screen = Screen::Phone;
        Ok(())
    }
}

fn describe(sent: &tl::types::auth::SentCode) -> String {
    use tl::enums::auth::SentCodeType as T;
    let how = match &sent.r#type {
        T::App(_) => "nell'app Telegram, chat «Telegram» (spunta blu), su un altro dispositivo già collegato".to_string(),
        T::Sms(_) | T::SmsWord(_) | T::SmsPhrase(_) | T::FirebaseSms(_) => "via SMS".to_string(),
        T::Call(_) => "con una telefonata".to_string(),
        T::FlashCall(_) | T::MissedCall(_) => "con una chiamata persa: il codice sono le ultime cifre del numero che ti chiama".to_string(),
        T::EmailCode(e) => format!("via email a {}", e.email_pattern),
        T::SetUpEmailRequired(_) => "da nessuna parte: Telegram chiede di impostare un'email di accesso. Accedi una volta dall'app ufficiale di Telegram (desktop o telefono), imposta l'email in Impostazioni → Privacy e sicurezza, poi riprova".to_string(),
        T::FragmentSms(f) => format!("su Fragment ({})", f.url),
    };
    let next = match &sent.next_type {
        Some(n) => format!(" Se non arriva puoi farlo rimandare ({n:?})."),
        None => String::new(),
    };
    format!("Telegram dice di averti mandato il codice {how}.{next}")
}

fn store_code(st: &Shared, phone: String, sent: tl::enums::auth::SentCode) -> Result<(), String> {
    let tl::enums::auth::SentCode::Code(sent) = sent else {
        return Err("Risposta inattesa da Telegram".into());
    };
    let mut s = st.lock().unwrap();
    s.code_info = describe(&sent);
    s.login = Some((phone, sent.phone_code_hash));
    s.screen = Screen::Code;
    Ok(())
}

// Login fatto a mano (non con request_login_code) per poter leggere dove Telegram manda il codice.
async fn send_code(st: Shared, client: Client, phone: String, api_hash: String) -> Result<(), String> {
    let (session, api_id) = {
        let s = st.lock().unwrap();
        (s.session.clone().ok_or("Non connesso")?, s.api_id)
    };
    let request = tl::functions::auth::SendCode {
        phone_number: phone.clone(),
        api_id,
        api_hash,
        settings: tl::types::CodeSettings {
            allow_flashcall: false,
            current_number: false,
            allow_app_hash: false,
            allow_missed_call: false,
            allow_firebase: false,
            logout_tokens: None,
            token: None,
            app_sandbox: None,
            unknown_number: false,
        }
        .into(),
    };
    let sent = match client.invoke(&request).await {
        // il numero appartiene a un altro datacenter: ci spostiamo lì e riproviamo
        Err(InvocationError::Rpc(e)) if e.code == 303 => {
            let dc = e.value.ok_or("Errore 303 senza datacenter")? as i32;
            session.set_home_dc_id(dc).await.map_err(err)?;
            client.invoke(&request).await.map_err(err)?
        }
        other => other.map_err(err)?,
    };
    store_code(&st, phone, sent)
}

async fn resend_code(st: Shared, client: Client) -> Result<(), String> {
    let (phone, hash) = st.lock().unwrap().login.clone().ok_or("Richiedi prima il codice")?;
    let request = tl::functions::auth::ResendCode { phone_number: phone.clone(), phone_code_hash: hash, reason: None };
    let sent = match client.invoke(&request).await {
        Err(e) if e.is("SEND_CODE_UNAVAILABLE") => {
            return Err("Telegram non ha altri modi per mandarti il codice: cercalo nella chat «Telegram» dell'app ufficiale, oppure usa il QR code".into());
        }
        other => other.map_err(err)?,
    };
    store_code(&st, phone, sent)
}

/// Chiede la password della verifica in due passaggi.
async fn ask_password(st: Shared, client: Client) -> Result<(), String> {
    let pw = client.invoke(&tl::functions::account::GetPassword {}).await.map_err(err)?;
    let t = PasswordToken::new(pw.into());
    let mut s = st.lock().unwrap();
    s.screen = Screen::Password(t.hint().unwrap_or("").to_string());
    s.password_token = Some(t);
    Ok(())
}

fn base64url(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = c.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..=c.len() {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Login col QR: lo inquadri dal telefono (Impostazioni → Dispositivi → Collega dispositivo desktop).
async fn qr_login(st: Shared, client: Client, api_hash: String) -> Result<(), String> {
    use tl::enums::auth::LoginToken as L;
    let (session, api_id, notify) = {
        let s = st.lock().unwrap();
        (s.session.clone().ok_or("Non connesso")?, s.api_id, s.login_update.clone())
    };
    loop {
        if !matches!(st.lock().unwrap().screen, Screen::Qr(_)) {
            return Ok(()); // l'utente è tornato indietro
        }
        let request = tl::functions::auth::ExportLoginToken { api_id, api_hash: api_hash.clone(), except_ids: vec![] };
        let result = match client.invoke(&request).await {
            Ok(L::MigrateTo(m)) => {
                // account su un altro datacenter: ci spostiamo lì e confermiamo il token
                session.set_home_dc_id(m.dc_id).await.map_err(err)?;
                client.invoke(&tl::functions::auth::ImportLoginToken { token: m.token }).await
            }
            other => other,
        };
        match result {
            Ok(L::Token(t)) => {
                st.lock().unwrap().screen = Screen::Qr(format!("tg://login?token={}", base64url(&t.token)));
                wake();
                // aspetta la scansione (arriva un update) o la scadenza del token, poi richiede
                let _ = tokio::time::timeout(std::time::Duration::from_secs(25), notify.notified()).await;
            }
            Ok(L::Success(_)) => return load_groups(st, client).await,
            Ok(L::MigrateTo(_)) => return Err("Risposta inattesa da Telegram".into()),
            Err(e) if e.is("SESSION_PASSWORD_NEEDED") => return ask_password(st, client).await,
            Err(e) => return Err(err(e)),
        }
    }
}

async fn sign_in(st: Shared, client: Client, code: String) -> Result<(), String> {
    let (phone, hash) = st.lock().unwrap().login.clone().ok_or("Richiedi prima il codice")?;
    let request = tl::functions::auth::SignIn {
        phone_number: phone,
        phone_code_hash: hash,
        phone_code: Some(code),
        email_verification: None,
    };
    match client.invoke(&request).await {
        Ok(tl::enums::auth::Authorization::Authorization(_)) => load_groups(st, client).await,
        Ok(tl::enums::auth::Authorization::SignUpRequired(_)) => {
            Err("Questo numero non ha un account Telegram: crealo prima dall'app ufficiale".into())
        }
        Err(e) if e.is("SESSION_PASSWORD_NEEDED") => ask_password(st, client).await,
        Err(e) if e.is("PHONE_CODE_*") => Err(format!("Codice errato o scaduto ({e})")),
        Err(e) => Err(err(e)),
    }
}

async fn check_password(st: Shared, client: Client, password: String) -> Result<(), String> {
    let token = st.lock().unwrap().password_token.take().ok_or("Sessione di login scaduta")?;
    match client.check_password(token, password.as_bytes()).await {
        Ok(_) => load_groups(st, client).await,
        Err(SignInError::InvalidPassword(t)) => {
            st.lock().unwrap().password_token = Some(t);
            Err("Password errata".into())
        }
        Err(e) => Err(err(e)),
    }
}

async fn load_groups(st: Shared, client: Client) -> Result<(), String> {
    let mut groups = Vec::new();
    let mut dialogs = client.iter_dialogs();
    while let Some(d) = dialogs.next().await.map_err(err)? {
        if !matches!(d.peer(), Peer::User(_)) {
            groups.push((d.peer().name().unwrap_or("?").to_string(), d.peer_ref()));
        }
    }
    let mut s = st.lock().unwrap();
    s.groups = groups;
    s.screen = Screen::Main;
    Ok(())
}

// shortcut: scorre tutti i messaggi; per gruppi enormi conviene search_messages con filtro media.
async fn scan(st: Shared, client: Client, peer: PeerRef, generation: u64) -> Result<(), String> {
    let mut messages = client.iter_messages(peer);
    let mut seen = 0;
    while let Some(m) = messages.next().await.map_err(err)? {
        let mut s = st.lock().unwrap();
        if s.scan_gen != generation {
            return Ok(());
        }
        if let Some(item) = m.media().and_then(|media| to_item(m.id(), media)) {
            s.files.push(item);
        }
        seen += 1;
        s.status = format!("Letti {seen} messaggi…");
        drop(s);
        if seen % 50 == 0 {
            wake();
        }
    }
    let mut s = st.lock().unwrap();
    if s.scan_gen == generation {
        s.scanning = false;
        s.status = format!("{} file in {seen} messaggi", s.files.len());
    }
    wake();
    Ok(())
}

async fn download(
    st: Shared,
    client: Client,
    items: Vec<(Media, String, usize)>,
    dir: PathBuf,
) -> Result<(), String> {
    std::fs::create_dir_all(&dir).map_err(err)?;
    let total = items.len();
    for (i, (media, name, size)) in items.into_iter().enumerate() {
        {
            let mut s = st.lock().unwrap();
            s.progress = Some((i, total));
            s.status = format!("Scarico {name}");
        }
        wake();
        let path = dir.join(&name);
        let mark = |st: &Shared| st.lock().unwrap().files.iter_mut().filter(|f| f.name == name).for_each(|f| f.done = true);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() as usize == size) {
            mark(&st);
            continue; // già scaricato
        }
        let tmp = dir.join(format!("{name}.part")); // niente file a metà se si interrompe
        // FLOOD_WAIT: Telegram limita la velocità e dice quanti secondi aspettare. Non si può
        // aggirare: aspettiamo con un conto alla rovescia e riscarichiamo lo stesso file.
        while let Err(e) = client.download_media(&media, &tmp).await {
            let InvocationError::Rpc(rpc) = &e else { return Err(err(e)) };
            let Some(wait) = rpc.value.filter(|_| rpc.code == 420) else { return Err(err(e)) };
            for left in (1..=wait + 1).rev() {
                st.lock().unwrap().status = format!("Telegram chiede di rallentare: riprendo {name} tra {left} s");
                wake();
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            st.lock().unwrap().status = format!("Scarico {name}");
        }
        std::fs::rename(&tmp, &path).map_err(err)?;
        mark(&st);
    }
    let mut s = st.lock().unwrap();
    s.progress = None;
    s.status = format!("Fatto: {total} file in {}", dir.display());
    Ok(())
}

// ---------- interfaccia (Rune) ----------

static WAKER: OnceLock<rune::Waker> = OnceLock::new();

/// Ridisegna l'interfaccia: lo chiamano i task in background quando cambiano lo stato.
fn wake() {
    if let Some(w) = WAKER.get() {
        w();
    }
}

struct App {
    rt: tokio::runtime::Runtime,
    st: Shared,
    api_id: TextEdit,
    api_hash: TextEdit,
    phone: TextEdit,
    code: TextEdit,
    password: TextEdit,
    group_filter: TextEdit,
    file_filter: TextEdit,
    kind_filter: Option<Kind>,
    out_dir: PathBuf,
}

type Task = std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

impl App {
    fn new() -> Self {
        let st = Arc::new(Mutex::new(State {
            screen: Screen::Setup,
            busy: false,
            error: None,
            status: String::new(),
            client: None,
            session: None,
            api_id: 0,
            login: None,
            code_info: String::new(),
            password_token: None,
            login_update: Arc::new(tokio::sync::Notify::new()),
            groups: Vec::new(),
            current: None,
            scan_gen: 0,
            scanning: false,
            files: Vec::new(),
            progress: None,
        }));
        // con argomenti (--snap, --scene…) Rune disegna senza finestra: niente dati salvati né rete
        let window = std::env::args().len() == 1;
        let saved = if window { std::fs::read_to_string(data_dir().join("config")).unwrap_or_default() } else { String::new() };
        let mut lines = saved.lines();
        let mut app = App {
            rt: tokio::runtime::Runtime::new().expect("runtime tokio"),
            st,
            api_id: TextEdit::new(lines.next().unwrap_or("")),
            api_hash: TextEdit::new(lines.next().unwrap_or("")),
            phone: TextEdit::new(""),
            code: TextEdit::new(""),
            password: TextEdit::new(""),
            group_filter: TextEdit::new(""),
            file_filter: TextEdit::new(""),
            kind_filter: None,
            out_dir: std::env::home_dir().unwrap_or_default().join("Downloads").join("Telegram"),
        };
        if app.api_id.text.parse::<i32>().is_ok() {
            app.submit(); // credenziali già salvate: entra direttamente
        }
        app
    }

    /// Lancia un'operazione in background: mentre gira `busy` è vero, gli errori finiscono in `error`.
    fn run(&self, task: Task) {
        let mut s = self.st.lock().unwrap();
        if s.busy {
            return;
        }
        s.busy = true;
        s.error = None;
        let st = self.st.clone();
        self.rt.spawn(async move {
            let result = task.await;
            let mut s = st.lock().unwrap();
            s.busy = false;
            if let Err(e) = result {
                s.error = Some(e);
                s.progress = None;
            }
            drop(s);
            wake();
        });
    }

    fn fail(&self, msg: &str) {
        self.st.lock().unwrap().error = Some(msg.to_string());
    }

    fn client(&self) -> Option<Client> {
        self.st.lock().unwrap().client.clone()
    }

    fn hash(&self) -> String {
        self.api_hash.text.trim().to_string()
    }

    /// Invio o pulsante principale della schermata di accesso.
    fn submit(&mut self) {
        let screen = self.st.lock().unwrap().screen.clone();
        let st = self.st.clone();
        if screen == Screen::Setup {
            return match self.api_id.text.trim().parse() {
                Ok(id) => self.run(Box::pin(connect(st, id, self.hash()))),
                Err(_) => self.fail("api_id deve essere un numero"),
            };
        }
        let Some(c) = self.client() else { return };
        match screen {
            Screen::Phone => {
                let phone: String = self.phone.text.chars().filter(|ch| !ch.is_whitespace()).collect();
                // senza prefisso Telegram può interpretare il numero come di un altro paese
                if phone.starts_with('+') {
                    self.run(Box::pin(send_code(st, c, phone, self.hash())));
                } else {
                    self.fail("Scrivi il numero con il prefisso internazionale, es. +39 333…");
                }
            }
            Screen::Code => self.run(Box::pin(sign_in(st, c, self.code.text.trim().to_string()))),
            Screen::Password(_) => {
                let pw = self.password.text.clone();
                self.password.clear();
                self.run(Box::pin(check_password(st, c, pw)));
            }
            _ => {}
        }
    }

    fn qr(&self) {
        let Some(c) = self.client() else { return };
        // non passa da run(): il login col QR resta in attesa a lungo e non deve bloccare l'interfaccia
        {
            let mut s = self.st.lock().unwrap();
            s.screen = Screen::Qr(String::new());
            s.error = None;
        }
        let st = self.st.clone();
        let task = qr_login(st.clone(), c, self.hash());
        self.rt.spawn(async move {
            if let Err(e) = task.await {
                let mut s = st.lock().unwrap();
                s.error = Some(e);
                s.screen = Screen::Phone;
            }
            wake();
        });
    }

    fn pick(&self, i: usize) {
        let Some(c) = self.client() else { return };
        let mut s = self.st.lock().unwrap();
        s.current = Some(i);
        s.files.clear();
        s.scan_gen += 1;
        s.scanning = true;
        let task = scan(self.st.clone(), c, s.groups[i].1, s.scan_gen);
        self.rt.spawn(async move {
            let _ = task.await;
        });
    }

    fn download(&self) {
        let Some(c) = self.client() else { return };
        let s = self.st.lock().unwrap();
        let Some(current) = s.current else { return };
        let items: Vec<_> =
            s.files.iter().filter(|f| f.selected).filter_map(|f| Some((f.media.clone()?, f.name.clone(), f.size))).collect();
        let dir = self.out_dir.join(safe(&s.groups[current].0));
        drop(s);
        if !items.is_empty() {
            self.run(Box::pin(download(self.st.clone(), c, items, dir)));
        }
    }
}

/// Indici dei file che passano i filtri (tipo e testo).
fn visible(a: &App, s: &State) -> Vec<usize> {
    let needle = a.file_filter.text.to_lowercase();
    (0..s.files.len())
        .filter(|&i| {
            let f = &s.files[i];
            a.kind_filter.is_none_or(|k| f.kind == k) && f.name.to_lowercase().contains(&needle)
        })
        .collect()
}

fn mb(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

// --- aspetto: carta, inchiostro, caratteri enormi (lo stile "poster" di Spotseek) ---

fn c(s: &str) -> Color {
    hex(s).unwrap_or(Color::BLACK)
}
fn paper() -> Color {
    c("#f3f2ee")
}
fn ink() -> Color {
    c("#0b0b0b")
}
fn muted() -> Color {
    c("#8d8d8a")
}
fn red() -> Color {
    c("#e8321e")
}
fn stripe(i: usize) -> Color {
    const S: [&str; 6] = ["#f7e81a", "#5fdcb4", "#ff3e9c", "#ff6b12", "#6c8cff", "#7fd45a"];
    c(S[i % S.len()])
}
fn mono(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(size, weight).mono()
}
fn display(size: f32) -> TextStyle {
    TextStyle::new(size, 400.0).family(Family::Named("Anton")).lh(size)
}

const BLINK: Cycle = Cycle { period: 0.9, phase: 0.0, stops: &[(0.0, 1.0), (0.55, 1.0), (0.56, 0.15), (1.0, 0.15)], still: 1.0, fps: 6.0 };

fn button(key: &str, label: &str, dark: bool) -> El {
    let (bg, fg) = if dark { (ink(), paper()) } else { (Color::TRANSPARENT, ink()) };
    row().key(key).h(46.0).px(20.0).center().fill(bg).border(2.0, ink()).cursor(Cursor::Pointer).role("button").label(label)
        .hover(red())
        .child(text(label).font(mono(14.0, 600.0)).color(fg))
}

fn link_text(key: &str, label: &str) -> El {
    text(label).key(key).font(mono(13.0, 600.0)).color(ink()).cursor(Cursor::Pointer).role("button").label(label)
}

fn rule() -> El {
    el().w_full().h(2.0).fill(ink())
}

fn input(key: &str, edit: &TextEdit, size: f32, hint: &str, get: fn(&mut App) -> &mut TextEdit) -> El {
    text_input(edit, mono(size, 500.0)).key(key).single_line().w_full().color(ink()).caret(ink(), red().alpha(0.25))
        .placeholder(hint, muted())
        .on_edit(get)
        .on_submit(|a: &mut App, _: &mut Tx| a.submit())
}

fn field(key: &str, label: &str, input: El) -> El {
    col().key(format!("{key}-field")).w_full().gap(6.0)
        .child(text(label).font(mono(11.0, 600.0)).color(muted()))
        .child(input)
        .child(el().w_full().h(3.0).fill(ink()))
}

/// Carta millimetrata sbiadita dietro a tutto.
fn grid(size: [f32; 2]) -> El {
    let (step, line) = (44.0, ink().alpha(0.055));
    let mut g = el().absolute(0.0, 0.0).size(size[0], size[1]).inert();
    let mut x = step;
    while x < size[0] {
        g = g.child(el().absolute(x, 0.0).size(1.0, size[1]).fill(line));
        x += step;
    }
    let mut y = step;
    while y < size[1] {
        g = g.child(el().absolute(0.0, y).size(size[0], 1.0).fill(line));
        y += step;
    }
    g
}

/// Righe di moduli scuri, già unite in orizzontale: (riga, colonna, lunghezza).
fn qr_runs(dark: &[bool], w: usize) -> Vec<(usize, usize, usize)> {
    let mut runs = Vec::new();
    for (y, line) in dark.chunks(w).enumerate() {
        let mut x = 0;
        while x < line.len() {
            let len = line[x..].iter().take_while(|d| **d).count();
            if len > 0 {
                runs.push((y, x, len));
            }
            x += len.max(1);
        }
    }
    runs
}

fn qr(url: &str) -> El {
    let Ok(code) = qrcode::QrCode::new(url) else { return el() };
    let (w, cell) = (code.width(), 5.0);
    let dark: Vec<bool> = code.to_colors().iter().map(|c| *c == qrcode::Color::Dark).collect();
    let side = (w + 8) as f32 * cell; // 4 moduli di margine bianco per lato
    let mut q = el().key("qr").size(side, side).fill(Color::WHITE).border(2.0, ink()).label("QR code di accesso");
    for (y, x, len) in qr_runs(&dark, w) {
        q = q.child(el().absolute((x + 4) as f32 * cell, (y + 4) as f32 * cell).size(len as f32 * cell, cell).fill(Color::BLACK));
    }
    q
}

fn view(a: &App, cx: &mut Cx<App>) -> El {
    let size = cx.layout_size("root").unwrap_or([1000.0, 700.0]);
    let s = a.st.lock().unwrap();
    // spazio per i pulsanti della finestra, che stanno sopra al contenuto
    let inset = cx.titlebar_inset().min(26.0);
    col().key("root").w_full().h_full().pt(inset).fill(paper())
        .child(grid(size))
        .child(if s.screen == Screen::Main { main_screen(a, &s, size) } else { login(a, &s, size) })
}

fn brand(s: &State) -> El {
    let mut dot = el().size(9.0, 9.0).radius(5.0).fill(if s.error.is_some() { red() } else { ink() });
    if s.busy || s.scanning {
        dot = dot.cycle(BLINK);
    }
    row().center_y().gap(10.0).child(dot).child(text("TELEGRAM DOWNLOADER").font(mono(13.0, 600.0)).color(ink()))
}

// --- accesso ---

fn login(a: &App, s: &State, size: [f32; 2]) -> El {
    let big = (size[0] / 7.0).clamp(72.0, 170.0);
    let mut form = col().key("form").w((size[0] - 96.0).min(560.0)).gap(20.0).pt(22.0);
    let title = match &s.screen {
        Screen::Setup => {
            form = form
                .child(link_text("apps", "→ PRENDI API_ID E API_HASH SU MY.TELEGRAM.ORG").on_press_tx(|_: &mut App, tx: &mut Tx| {
                    let _ = tx.open_url("https://my.telegram.org/apps");
                }))
                .child(field("api-id", "API_ID", input("api-id", &a.api_id, 20.0, "1234567", |a| &mut a.api_id)))
                .child(field("api-hash", "API_HASH", input("api-hash", &a.api_hash, 20.0, "", |a| &mut a.api_hash).secure()))
                .child(row().child(button("go", "CONTINUA →", true).on_press(App::submit)));
            "CHIAVI API"
        }
        Screen::Phone => {
            form = form
                .child(row().child(button("qr-login", "ACCEDI CON QR CODE (CONSIGLIATO)", true).on_press(|a: &mut App| a.qr())))
                .child(text("OPPURE").font(mono(11.0, 600.0)).color(muted()))
                .child(field("phone", "NUMERO DI TELEFONO", input("phone", &a.phone, 20.0, "+39 333 1234567", |a| &mut a.phone)))
                .child(row().child(button("go", "INVIA CODICE →", false).on_press(App::submit)));
            "ACCEDI"
        }
        Screen::Code => {
            form = form
                .child(text(s.code_info.clone()).font(mono(13.0, 500.0)).color(ink()).wrap())
                .child(field("code", "CODICE", input("code", &a.code, 28.0, "12345", |a| &mut a.code)))
                .child(row().gap(12.0)
                    .child(button("go", "ACCEDI →", true).on_press(App::submit))
                    .child(button("resend", "RIMANDA IL CODICE", false).on_press(|a: &mut App| {
                        if let Some(c) = a.client() {
                            a.run(Box::pin(resend_code(a.st.clone(), c)));
                        }
                    })))
                .child(row().gap(24.0)
                    .child(link_text("use-qr", "USA IL QR CODE").on_press(|a: &mut App| a.qr()))
                    .child(link_text("back", "← NUMERO SBAGLIATO").on_press(|a: &mut App| a.st.lock().unwrap().screen = Screen::Phone)));
            "CODICE"
        }
        Screen::Qr(url) => {
            form = form
                .child(text("SUL TELEFONO: IMPOSTAZIONI → DISPOSITIVI → COLLEGA DISPOSITIVO DESKTOP").font(mono(13.0, 500.0)).color(ink()).wrap())
                .child(if url.is_empty() { text("PREPARO IL QR…").font(mono(13.0, 600.0)).color(muted()).cycle(BLINK) } else { qr(url) })
                .child(link_text("back", "← TORNA INDIETRO").on_press(|a: &mut App| a.st.lock().unwrap().screen = Screen::Phone));
            "INQUADRA"
        }
        Screen::Password(hint) => {
            let label = if hint.is_empty() { "VERIFICA IN DUE PASSAGGI".to_string() } else { format!("VERIFICA IN DUE PASSAGGI · SUGGERIMENTO: {hint}") };
            form = form
                .child(field("password", &label, input("password", &a.password, 20.0, "", |a| &mut a.password).secure()))
                .child(row().child(button("go", "ACCEDI →", true).on_press(App::submit)));
            "PASSWORD"
        }
        Screen::Main => "",
    };
    let note = match &s.error {
        Some(e) => text(e.clone()).key("note").font(mono(13.0, 600.0)).color(red()).wrap(),
        None if s.busy => text("UN ATTIMO…").key("note").font(mono(13.0, 600.0)).color(muted()).cycle(BLINK),
        None => text("").key("note"),
    };
    col().key("login").w_full().h_full().px(48.0).pt(22.0).pb(26.0)
        .child(brand(s))
        .child(text(title).key("headline").font(display(big)).color(ink()).role("heading").mt(26.0).mb(8.0))
        .child(rule())
        .child(form)
        .child(spacer())
        .child(note)
}

// --- gruppi e file ---

fn stat(label: String) -> El {
    row().center_y().gap(16.0).child(el().size(2.0, 18.0).fill(ink())).child(text(label).font(mono(13.0, 600.0)).color(ink()))
}

fn main_screen(a: &App, s: &State, size: [f32; 2]) -> El {
    let chosen = s.files.iter().filter(|f| f.selected).count();
    let bytes: usize = s.files.iter().filter(|f| f.selected).map(|f| f.size).sum();

    let header = row().key("header").w_full().h(50.0).px(24.0).center_y().gap(16.0)
        .child(brand(s))
        .child(stat(format!("{} GRUPPI", s.groups.len())))
        .child(stat(format!("{} FILE", s.files.len())))
        .child(stat(format!("{chosen} SCELTI · {}", mb(bytes))));

    // elenco gruppi
    let needle = a.group_filter.text.to_lowercase();
    let mut groups = col().key("group-rows").w_full();
    for (i, (name, _)) in s.groups.iter().enumerate().filter(|(_, g)| g.0.to_lowercase().contains(&needle)) {
        let on = s.current == Some(i);
        groups = groups.child(row().key(format!("g{i}")).w_full().h(30.0).px(10.0).center_y()
            .fill(if on { ink() } else { Color::TRANSPARENT }).hover(red()).cursor(Cursor::Pointer).role("button").label(name.clone())
            .on_press(move |a: &mut App| a.pick(i))
            .child(text(name.to_uppercase()).font(mono(12.5, 600.0)).color(if on { paper() } else { ink() }).truncate().grow(1.0).min_w(0.0)));
    }
    let side = col().key("side").w(270.0).h_full().min_h(0.0)
        .child(col().w_full().px(14.0).pt(12.0).pb(10.0).gap(6.0)
            .child(input("group-filter", &a.group_filter, 14.0, "CERCA GRUPPO…", |a| &mut a.group_filter))
            .child(rule()))
        .child(col().key("groups").w_full().grow(1.0).min_h(0.0).pb(8.0).scroll(ScrollSpec { stick_bottom: false }).child(groups));

    let body = row().key("body").w_full().grow(1.0).min_h(0.0)
        .child(side)
        .child(el().w(2.0).h_full().fill(ink()))
        .child(files(a, s, size[0] - 272.0));

    // piede: avanzamento, stato, cartella, scarica
    let mut foot = col().key("foot").w_full();
    let mut bar = row().key("bar").w_full().h(62.0).px(24.0).center_y().gap(18.0);
    if let Some((done, total)) = s.progress {
        let p = done as f32 / total.max(1) as f32;
        foot = foot.child(el().key("progress").w_full().h(10.0).fill(ink().alpha(0.1)).child(el().h_full().w_frac(p).fill(red())));
        bar = bar.child(text(format!("{:.0}%", p * 100.0)).font(display(38.0)).color(ink()))
            .child(text(format!("{done}/{total}")).font(mono(13.0, 600.0)).color(ink()));
    }
    let (note, color) = match &s.error {
        Some(e) => (e.clone(), red()),
        None => (s.status.to_uppercase(), ink()),
    };
    let ready = chosen > 0 && !s.busy;
    let home = std::env::home_dir().unwrap_or_default();
    let folder = a.out_dir.strip_prefix(&home).map_or(a.out_dir.display().to_string(), |p| format!("~/{}", p.display()));
    bar = bar
        .child(text(note).key("status").font(mono(12.0, 600.0)).color(color).truncate().grow(1.0).min_w(0.0))
        .child(link_text("folder", &format!("CARTELLA: {} ↗", folder)).truncate().max_w(300.0).on_press_tx(|a: &mut App, tx: &mut Tx| {
            let options = FileDialogOptions { title: "Cartella di destinazione".into(), directory: Some(a.out_dir.clone()), ..Default::default() };
            tx.open_folder(options, |a: &mut App, result, _: &mut Tx| match result {
                Ok(Some(dir)) => a.out_dir = dir,
                Ok(None) => {}
                Err(e) => a.fail(&e.to_string()),
            });
        }))
        .child(button("download", &format!("SCARICA {chosen} FILE ↓"), true).opacity(if ready { 1.0 } else { 0.3 }).on_press(|a: &mut App| a.download()));
    foot = foot.child(bar);

    col().key("main").w_full().h_full().min_h(0.0).child(header).child(rule()).child(body).child(rule()).child(foot)
}

fn chip(label: &'static str, kind: Option<Kind>, on: bool) -> El {
    row().key(label).h(30.0).px(12.0).center().border(2.0, ink()).fill(if on { ink() } else { Color::TRANSPARENT })
        .hover(red()).cursor(Cursor::Pointer).role("button").label(label)
        .on_press(move |a: &mut App| a.kind_filter = kind)
        .child(text(label).font(mono(12.0, 600.0)).color(if on { paper() } else { ink() }))
}

fn files(a: &App, s: &State, width: f32) -> El {
    let panel = col().key("files").grow(1.0).min_w(0.0).h_full().min_h(0.0);
    let Some(current) = s.current else {
        return panel.px(28.0).pt(18.0)
            .child(text("SCEGLI").font(display((width / 5.0).clamp(80.0, 190.0))).color(ink()))
            .child(text("UN GRUPPO").font(display((width / 5.0).clamp(80.0, 190.0))).color(red()))
            .child(text("← A SINISTRA CI SONO I TUOI GRUPPI E CANALI").font(mono(13.0, 600.0)).color(muted()).mt(14.0));
    };
    let shown = visible(a, s);
    let mut chips = row().key("chips").w_full().center_y().gap(8.0);
    for (label, k) in [("TUTTI", None), ("FOTO", Some(Kind::Foto)), ("VIDEO", Some(Kind::Video)), ("AUDIO", Some(Kind::Audio)), ("DOCUMENTI", Some(Kind::Documento))] {
        chips = chips.child(chip(label, k, a.kind_filter == k));
    }
    chips = chips.child(col().grow(1.0).min_w(0.0).pl(10.0).gap(4.0)
        .child(input("file-filter", &a.file_filter, 14.0, "CERCA FILE…", |a| &mut a.file_filter))
        .child(rule()));

    let actions = row().key("actions").w_full().center_y().gap(22.0)
        .child(link_text("all", "SELEZIONA TUTTI").on_press(|a: &mut App| {
            let mut s = a.st.lock().unwrap();
            for i in visible(a, &s) {
                s.files[i].selected = true;
            }
        }))
        .child(link_text("none", "DESELEZIONA").on_press(|a: &mut App| a.st.lock().unwrap().files.iter_mut().for_each(|f| f.selected = false)))
        .child(spacer())
        .child(text(format!("{} IN ELENCO", shown.len())).font(mono(12.0, 600.0)).color(muted()));

    // righe: una striscia colorata per ogni file scelto
    let rows = shown.iter().map(|&i| {
        let f = &s.files[i];
        let kind = match f.kind {
            Kind::Foto => "FOTO",
            Kind::Video => "VIDEO",
            Kind::Audio => "AUDIO",
            Kind::Documento => "DOC",
        };
        let fg = if f.selected || f.done { ink() } else { muted() };
        row().key(format!("f{i}")).w_full().h(28.0).px(8.0).center_y()
            .fill(if f.selected { stripe(i) } else { Color::TRANSPARENT })
            .cursor(Cursor::Pointer).role("checkbox").checked(f.selected).label(f.name.clone())
            .on_press(move |a: &mut App| {
                if let Some(f) = a.st.lock().unwrap().files.get_mut(i) {
                    f.selected = !f.selected;
                }
            })
            .child(el().size(12.0, 12.0).mr(10.0).border(2.0, ink()).fill(if f.selected { ink() } else { Color::TRANSPARENT }))
            .child(text(f.name.clone()).font(mono(12.5, 600.0)).color(ink()).truncate().grow(1.0).min_w(0.0))
            .child(text(if f.done { "OK" } else { "" }).font(mono(11.0, 600.0)).color(ink()).ml(8.0))
            .child(row().w(64.0).justify_end().child(text(kind).font(mono(11.0, 600.0)).color(fg)))
            .child(row().w(92.0).justify_end().child(text(mb(f.size)).font(mono(11.0, 600.0)).color(fg)))
    });
    let list = if shown.is_empty() {
        let note = if s.scanning { "LEGGO I MESSAGGI…" } else { "NESSUN FILE QUI" };
        col().w_full().grow(1.0).pt(18.0).child(text(note).font(mono(13.0, 600.0)).color(muted()))
    } else {
        col().key("file-list").w_full().grow(1.0).min_h(0.0).pb(8.0).scroll(ScrollSpec { stick_bottom: false })
            .child(col().key("rows").w_full().growth(GrowthSpec::default()).virtualize(28.0).children(rows))
    };

    panel.px(24.0).pt(14.0).gap(12.0)
        .child(text(s.groups[current].0.to_uppercase()).key("group-name").font(display((width / 11.0).clamp(44.0, 84.0))).color(ink()).truncate().role("heading"))
        .child(chips)
        .child(actions)
        .child(rule())
        .child(list)
}

// --- scene di esempio: `telegram-downloader --snap out.png --scene files` disegna senza finestra ---

fn scene_code(a: &mut App) {
    let mut s = a.st.lock().unwrap();
    s.screen = Screen::Code;
    s.code_info = "Telegram dice di averti mandato il codice nell'app Telegram, chat «Telegram» (spunta blu), su un altro dispositivo già collegato.".into();
}

fn scene_qr(a: &mut App) {
    a.st.lock().unwrap().screen = Screen::Qr("tg://login?token=AQJ9cXNn8mJ3bq1uY0t5c2xkZmFrZXRva2VuZXhhbXBsZTEyMzQ1Ng".into());
}

fn scene_groups(a: &mut App) {
    use grammers_session::types::PeerId;
    let mut s = a.st.lock().unwrap();
    s.screen = Screen::Main;
    s.groups = ["Foto di famiglia", "Viaggio in Giappone 2025", "Calcetto del giovedì", "Ricette della nonna", "Appunti universitari", "Colleghi", "Cinema d'essai", "Vinili e giradischi", "Condominio via Roma", "Trekking Dolomiti", "Podcast del lunedì", "Libri in PDF"]
        .iter()
        .map(|n| (n.to_string(), PeerId::chat_unchecked(1).to_ambient_ref()))
        .collect();
}

fn scene_files(a: &mut App) {
    scene_groups(a);
    let mut s = a.st.lock().unwrap();
    s.current = Some(1);
    const NAMES: [(&str, Kind); 8] = [
        ("kyoto_fushimi_inari.jpg", Kind::Foto), ("shibuya_di_notte.mp4", Kind::Video), ("itinerario.pdf", Kind::Documento), ("tempio_d_oro.jpg", Kind::Foto),
        ("karaoke.m4a", Kind::Audio), ("shinkansen.mp4", Kind::Video), ("monte_fuji_alba.jpg", Kind::Foto), ("biglietti_treno.pdf", Kind::Documento),
    ];
    s.files = (0..48usize)
        .map(|i| {
            let (name, kind) = NAMES[i % NAMES.len()];
            let size = 600_000 + (i * 7_919_003) % if kind == Kind::Video { 180_000_000 } else { 9_000_000 };
            FileItem { name: format!("{}_{name}", 4812 - i * 3), kind, size, media: None, selected: i % 3 != 2 && i < 30, done: i < 7 && i % 3 != 2 }
        })
        .collect();
    s.status = "48 file in 1312 messaggi".into();
}

fn scene_downloading(a: &mut App) {
    scene_files(a);
    let mut s = a.st.lock().unwrap();
    s.busy = true;
    s.progress = Some((5, 20));
    s.status = "Scarico 4797_shinkansen.mp4".into();
}

fn main() {
    rune::prewarm_gpu();
    rune::run(AppSpec::<App> {
        min_size: Some((820.0, 560.0)),
        scenes: &[
            ("phone", |a| a.st.lock().unwrap().screen = Screen::Phone),
            ("code", scene_code),
            ("qr", scene_qr),
            ("password", |a| a.st.lock().unwrap().screen = Screen::Password("il gatto".into())),
            ("groups", scene_groups),
            ("files", scene_files),
            ("downloading", scene_downloading),
        ],
        fonts: &[
            include_bytes!("../assets/IBMPlexMono-Regular.ttf"),
            include_bytes!("../assets/IBMPlexMono-Medium.ttf"),
            include_bytes!("../assets/IBMPlexMono-SemiBold.ttf"),
            include_bytes!("../assets/Anton-Regular.ttf"),
        ],
        families: ("IBM Plex Mono", "IBM Plex Mono"),
        setup: Some(|_, waker| {
            let _ = WAKER.set(waker);
        }),
        // lo stato vero sta dietro un Mutex che cambiano i task: a ogni risveglio si ridisegna
        poll: Some(|_, _, _| true),
        ..AppSpec::new("Telegram Downloader", (1000.0, 700.0), App::new, view)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

    #[test]
    fn qr_unisce_i_moduli_vicini() {
        let d = [true, true, false, true, false, false, true, true];
        assert_eq!(qr_runs(&d, 4), vec![(0, 0, 2), (0, 3, 1), (1, 2, 2)]);
    }

    #[test]
    fn un_clic_sceglie_il_file() {
        let mut app = App::new();
        scene_files(&mut app);
        app.st.lock().unwrap().files.iter_mut().for_each(|f| f.selected = false);
        let mut rt = Runtime::new(app, view, (1000.0, 700.0), Env { system_dark: false, scale: 1.0 });
        rt.frame(0.0);
        rt.frame(0.1);
        let (x, y) = rt.find("f0").expect("la prima riga è sullo schermo");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.2);
        assert!(rt.state.st.lock().unwrap().files[0].selected);
    }


    #[test]
    fn base64_url() {
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn nomi_sicuri() {
        assert_eq!(file_name(5, Some("foto.jpg"), "image/jpeg"), "5_foto.jpg");
        assert_eq!(file_name(5, None, "video/mp4"), "5_file.mp4");
        assert_eq!(file_name(5, Some("../../etc/passwd"), ""), "5_.._.._etc_passwd");
        assert_eq!(file_name(5, Some(".."), ""), "5_file.bin");
        assert_eq!(file_name(5, Some("a:b?.txt"), ""), "5_a_b_.txt");
        assert_eq!(file_name(5, Some(r"..\..\x.exe"), ""), "5_.._.._x.exe");
        assert_eq!(file_name(5, Some("C:x.exe"), ""), "5_C_x.exe");
    }
}
