//! GUI per scaricare i file dei gruppi Telegram: login, scelta del gruppo, scelta dei file.
#![cfg_attr(windows, windows_subsystem = "windows")] // su Windows niente finestra di console
use std::fmt::Display;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui;
use grammers_client::client::PasswordToken;
use grammers_client::media::Media;
use grammers_client::peer::Peer;
use grammers_client::{Client, InvocationError, SenderPool, SignInError, tl};
use grammers_session::Session;
use grammers_session::storages::SqliteSession;
use grammers_session::types::PeerRef;

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
    media: Media,
    selected: bool,
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
    Some(FileItem { name, kind, size, media, selected: false })
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
async fn scan(st: Shared, ctx: egui::Context, client: Client, peer: PeerRef, generation: u64) -> Result<(), String> {
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
            ctx.request_repaint();
        }
    }
    let mut s = st.lock().unwrap();
    if s.scan_gen == generation {
        s.scanning = false;
        s.status = format!("{} file in {seen} messaggi", s.files.len());
    }
    Ok(())
}

async fn download(
    st: Shared,
    ctx: egui::Context,
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
        ctx.request_repaint();
        let path = dir.join(&name);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() as usize == size) {
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
                ctx.request_repaint();
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            st.lock().unwrap().status = format!("Scarico {name}");
        }
        std::fs::rename(&tmp, &path).map_err(err)?;
    }
    let mut s = st.lock().unwrap();
    s.progress = None;
    s.status = format!("Fatto: {total} file in {}", dir.display());
    Ok(())
}

// ---------- interfaccia ----------

struct App {
    rt: tokio::runtime::Runtime,
    st: Shared,
    api_id: String,
    api_hash: String,
    phone: String,
    code: String,
    password: String,
    group_filter: String,
    file_filter: String,
    kind_filter: Option<Kind>,
    out_dir: PathBuf,
}

type Task = std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// Lancia un'operazione in background: mentre gira `busy` è vero, gli errori finiscono in `error`.
fn run(rt: &tokio::runtime::Runtime, st: &Shared, s: &mut State, ctx: &egui::Context, task: Task) {
    s.busy = true;
    s.error = None;
    let (st, ctx) = (st.clone(), ctx.clone());
    rt.spawn(async move {
        let result = task.await;
        let mut s = st.lock().unwrap();
        s.busy = false;
        if let Err(e) = result {
            s.error = Some(e);
            s.progress = None;
        }
        ctx.request_repaint();
    });
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
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
        let saved = std::fs::read_to_string(data_dir().join("config")).unwrap_or_default();
        let mut lines = saved.lines();
        let app = App {
            rt: tokio::runtime::Runtime::new().expect("runtime tokio"),
            st,
            api_id: lines.next().unwrap_or("").to_string(),
            api_hash: lines.next().unwrap_or("").to_string(),
            phone: String::new(),
            code: String::new(),
            password: String::new(),
            group_filter: String::new(),
            file_filter: String::new(),
            kind_filter: None,
            out_dir: std::env::home_dir().unwrap_or_default().join("Downloads").join("Telegram"),
        };
        if let Ok(id) = app.api_id.parse() {
            // credenziali già salvate: entra direttamente
            let task = Box::pin(connect(app.st.clone(), id, app.api_hash.clone()));
            run(&app.rt, &app.st, &mut app.st.lock().unwrap(), ctx, task);
        }
        app
    }
}

fn form_row(ui: &mut egui::Ui, label: &str, value: &mut String, password: bool) -> bool {
    ui.label(label);
    let r = ui.add(egui::TextEdit::singleline(value).password(password).desired_width(280.0));
    r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

fn mb(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let st = self.st.clone();
        let mut s = st.lock().unwrap();
        let client = s.client.clone();

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if s.busy || s.scanning {
                    ui.spinner();
                }
                ui.label(&s.status);
            });
            if let Some((done, total)) = s.progress {
                ui.add(egui::ProgressBar::new(done as f32 / total as f32).text(format!("{done}/{total}")));
            }
            if let Some(e) = &s.error {
                ui.colored_label(egui::Color32::RED, e);
            }
        });

        let mut task: Option<Task> = None;
        if s.screen != Screen::Main {
            let screen = s.screen.clone();
            let busy = s.busy;
            let mut error = None;
            let mut back = false;
            let mut qr = false;
            let code_info = s.code_info.clone();
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.heading("Telegram downloader");
                    ui.add_space(20.0);
                    ui.add_enabled_ui(!busy, |ui| match screen.clone() {
                        Screen::Setup => {
                            ui.hyperlink_to("Prendi api_id e api_hash su my.telegram.org", "https://my.telegram.org/apps");
                            ui.add_space(8.0);
                            let mut go = form_row(ui, "api_id", &mut self.api_id, false);
                            go |= form_row(ui, "api_hash", &mut self.api_hash, true);
                            if ui.button("Continua").clicked() || go {
                                self.api_hash = self.api_hash.trim().to_string();
                                match self.api_id.trim().parse() {
                                    Ok(id) => task = Some(Box::pin(connect(st.clone(), id, self.api_hash.clone()))),
                                    Err(_) => error = Some("api_id deve essere un numero".to_string()),
                                }
                            }
                        }
                        Screen::Phone => {
                            if ui.button("Accedi con QR code (consigliato)").clicked() {
                                qr = true;
                            }
                            ui.label("oppure");
                            let go = form_row(ui, "Numero di telefono (es. +39…)", &mut self.phone, false);
                            if (ui.button("Invia codice").clicked() || go) && let Some(c) = client.clone() {
                                let phone: String = self.phone.chars().filter(|ch| !ch.is_whitespace()).collect();
                                // senza prefisso Telegram può interpretare il numero come di un altro paese
                                if phone.starts_with('+') {
                                    task = Some(Box::pin(send_code(st.clone(), c, phone, self.api_hash.clone())));
                                } else {
                                    error = Some("Scrivi il numero con il prefisso internazionale, es. +39 333…".to_string());
                                }
                            }
                        }
                        Screen::Code => {
                            ui.add(egui::Label::new(&code_info).wrap());
                            ui.add_space(8.0);
                            let go = form_row(ui, "Codice", &mut self.code, false);
                            if (ui.button("Accedi").clicked() || go) && let Some(c) = client.clone() {
                                task = Some(Box::pin(sign_in(st.clone(), c, self.code.trim().to_string())));
                            }
                            if ui.button("Rimanda il codice").clicked() && let Some(c) = client.clone() {
                                task = Some(Box::pin(resend_code(st.clone(), c)));
                            }
                            if ui.link("Usa il QR code invece").clicked() {
                                qr = true;
                            }
                            if ui.link("Numero sbagliato? Torna indietro").clicked() {
                                back = true;
                            }
                        }
                        Screen::Qr(_) => {}
                        Screen::Password(hint) => {
                            let label = format!("Password verifica in due passaggi (suggerimento: {hint})");
                            let go = form_row(ui, &label, &mut self.password, true);
                            if (ui.button("Accedi").clicked() || go) && let Some(c) = client.clone() {
                                let pw = std::mem::take(&mut self.password);
                                task = Some(Box::pin(check_password(st.clone(), c, pw)));
                            }
                        }
                        Screen::Main => {}
                    });
                    // il QR sta fuori da add_enabled_ui: da disabilitato verrebbe sbiadito e illeggibile
                    if let Screen::Qr(url) = &screen {
                        ui.label("Sul telefono: Impostazioni → Dispositivi → Collega dispositivo desktop, poi inquadra:");
                        ui.add_space(8.0);
                        if let Ok(code) = qrcode::QrCode::new(url) {
                            let (w, cell) = (code.width(), 6.0);
                            let side = (w + 8) as f32 * cell; // 4 moduli di margine bianco per lato
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
                            let p = ui.painter();
                            p.rect_filled(rect, 0.0, egui::Color32::WHITE);
                            for (i, c) in code.to_colors().iter().enumerate() {
                                if *c == qrcode::Color::Dark {
                                    let min = rect.min + egui::vec2((i % w + 4) as f32, (i / w + 4) as f32) * cell;
                                    p.rect_filled(egui::Rect::from_min_size(min, egui::vec2(cell, cell)), 0.0, egui::Color32::BLACK);
                                }
                            }
                        }
                        ui.add_space(8.0);
                        if ui.link("Torna indietro").clicked() {
                            back = true;
                        }
                    }
                });
            });
            if error.is_some() {
                s.error = error;
            }
            if back {
                s.screen = Screen::Phone;
            }
            if qr && let Some(c) = client.clone() {
                // non passa da run(): il login col QR resta in attesa a lungo e non deve bloccare l'interfaccia
                s.screen = Screen::Qr(String::new());
                s.error = None;
                let st2 = st.clone();
                let task = qr_login(st.clone(), c, self.api_hash.clone());
                self.rt.spawn(async move {
                    if let Err(e) = task.await {
                        let mut s = st2.lock().unwrap();
                        s.error = Some(e);
                        s.screen = Screen::Phone;
                    }
                });
            }
            if let Some(t) = task {
                run(&self.rt, &st, &mut s, &ctx, t);
            }
            return;
        }

        // --- elenco gruppi ---
        let mut picked = None;
        egui::Panel::left("groups").default_size(260.0).show(ui, |ui| {
            ui.add_space(6.0);
            ui.add(egui::TextEdit::singleline(&mut self.group_filter).hint_text("Cerca gruppo…"));
            ui.separator();
            let needle = self.group_filter.to_lowercase();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (i, (name, _)) in s.groups.iter().enumerate() {
                    if name.to_lowercase().contains(&needle)
                        && ui.selectable_label(s.current == Some(i), name).clicked()
                    {
                        picked = Some(i);
                    }
                }
            });
        });
        if let (Some(i), Some(c)) = (picked, client.clone()) {
            s.current = Some(i);
            s.files.clear();
            s.scan_gen += 1;
            s.scanning = true;
            let task = scan(st.clone(), ctx.clone(), c, s.groups[i].1, s.scan_gen);
            self.rt.spawn(async move { task.await });
        }

        // --- file del gruppo ---
        egui::CentralPanel::default().show(ui, |ui| {
            let Some(current) = s.current else {
                ui.centered_and_justified(|ui| ui.label("Scegli un gruppo a sinistra"));
                return;
            };
            ui.heading(&s.groups[current].0);

            ui.horizontal(|ui| {
                for (label, k) in [("Tutti", None), ("Foto", Some(Kind::Foto)), ("Video", Some(Kind::Video)),
                    ("Audio", Some(Kind::Audio)), ("Documenti", Some(Kind::Documento))]
                {
                    ui.selectable_value(&mut self.kind_filter, k, label);
                }
                ui.add(egui::TextEdit::singleline(&mut self.file_filter).hint_text("Cerca file…"));
            });

            let needle = self.file_filter.to_lowercase();
            let visible: Vec<usize> = (0..s.files.len())
                .filter(|&i| {
                    let f = &s.files[i];
                    self.kind_filter.is_none_or(|k| f.kind == k) && f.name.to_lowercase().contains(&needle)
                })
                .collect();

            ui.horizontal(|ui| {
                if ui.button("Seleziona tutti").clicked() {
                    visible.iter().for_each(|&i| s.files[i].selected = true);
                }
                if ui.button("Deseleziona").clicked() {
                    s.files.iter_mut().for_each(|f| f.selected = false);
                }
                ui.label(format!("Cartella: {}", self.out_dir.display()));
                if ui.button("Cambia…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().set_directory(&self.out_dir).pick_folder()
                {
                    self.out_dir = dir;
                }
            });

            let chosen: Vec<&FileItem> = s.files.iter().filter(|f| f.selected).collect();
            let size: usize = chosen.iter().map(|f| f.size).sum();
            let label = format!("Scarica {} file ({})", chosen.len(), mb(size));
            let enabled = !chosen.is_empty() && !s.busy;
            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() && let Some(c) = client.clone() {
                let items = chosen.iter().map(|f| (f.media.clone(), f.name.clone(), f.size)).collect();
                let dir = self.out_dir.join(safe(&s.groups[current].0));
                task = Some(Box::pin(download(st.clone(), ctx.clone(), c, items, dir)));
            }
            ui.separator();

            let row_h = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, row_h, visible.len(), |ui, range| {
                for &i in &visible[range] {
                    let f = &mut s.files[i];
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut f.selected, &f.name);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(mb(f.size)));
                    });
                }
            });
        });
        if let Some(t) = task {
            run(&self.rt, &st, &mut s, &ctx, t);
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([960.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native("Telegram downloader", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

#[cfg(test)]
mod tests {
    use super::{base64url, file_name};

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
