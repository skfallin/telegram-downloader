<p align="center"><img src="assets/logo.png" width="128" alt="Logo di Telegram Downloader"></p>
<h1 align="center">Telegram Downloader</h1>
<p align="center">Scarica tutti i file di un gruppo o canale Telegram di cui fai parte, usando il tuo account.</p>
<p align="center">
  <a href="https://github.com/skfallin/telegram-downloader/releases/latest"><img src="https://img.shields.io/github/v/release/skfallin/telegram-downloader?label=download&color=e8321e" alt="Ultima versione"></a>
  <a href="https://github.com/skfallin/telegram-downloader/actions/workflows/release.yml"><img src="https://github.com/skfallin/telegram-downloader/actions/workflows/release.yml/badge.svg" alt="Build"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/licenza-MIT-0b0b0b" alt="Licenza MIT"></a>
  <img src="https://img.shields.io/badge/Windows%20·%20macOS%20·%20Linux-f7e81a" alt="Windows, macOS, Linux">
</p>

![Scelta dei file di un gruppo](assets/screenshot-files.png)

| Accesso con QR code | Download in corso |
|---|---|
| ![Accesso con QR code](assets/screenshot-qr.png) | ![Download in corso](assets/screenshot-downloading.png) |

- **Tutto in un'app sola**: niente Python, niente terminale. Un eseguibile per Windows, macOS e Linux.
- **Accesso sicuro**: QR code o numero + codice, con verifica in due passaggi. La sessione resta solo sul tuo computer.
- **Scegli cosa scaricare**: filtra per foto, video, audio, documenti o per nome.
- **Riprende da solo**: salta i file già scaricati e aspetta quando Telegram chiede di rallentare.

## App con interfaccia grafica (Windows, macOS, Linux)

Scarica l'ultima versione da [Releases](https://github.com/skfallin/telegram-downloader/releases/latest):

| Sistema | File | Come aprirla |
|---|---|---|
| Windows | `telegram-downloader-windows-x64.exe` | Doppio clic. Se compare "Windows ha protetto il PC": **Ulteriori informazioni → Esegui comunque**. |
| macOS (Apple Silicon e Intel) | `telegram-downloader-macos.zip` | Estrai e sposta **Telegram Downloader** in Applicazioni. La prima volta: tasto destro → **Apri** (l'app non è firmata da Apple). |
| Linux | `telegram-downloader-linux-x64.tar.gz` | `tar -xzf telegram-downloader-linux-x64.tar.gz && ./telegram-downloader` |

Uso:

1. Inserisci `api_id` e `api_hash` (da <https://my.telegram.org/apps>), poi accedi con il **QR code** (dal telefono: Impostazioni → Dispositivi → Collega dispositivo desktop) oppure con numero e codice. Se ce l'hai, ti chiede la password della verifica in due passaggi.
2. Scegli un gruppo a sinistra: l'app legge i messaggi e mostra i file man mano.
3. Filtra per tipo o per nome, spunta i file (o "Seleziona tutti"), scegli la cartella e premi **Scarica**.

I file finiscono in `<cartella>/<nome gruppo>/`. Se riscarichi, quelli già presenti vengono saltati.
Se Telegram chiede di rallentare (FLOOD_WAIT), l'app aspetta da sola e riprende.
Credenziali e sessione stanno nella cartella `.telegram-downloader` della tua home: per uscire dall'account cancellala. **Non condividerla**, dà accesso al tuo account.

### Compilarla da sé

Serve [Rust](https://rustup.rs). Su Linux anche `pkg-config libdbus-1-dev libwayland-dev libxkbcommon-dev libgl1-mesa-dev xorg-dev`.

```bash
cd gui && cargo run --release
```
Su macOS `gui/install.sh` la compila e la mette in Applicazioni.

`telegram-downloader --snap schermata.png --scene files` disegna una schermata di esempio senza aprire la finestra (le scene: `--list-scenes`).

Per pubblicare una nuova versione: `git tag vX.Y.Z && git push --tags`. GitHub Actions compila per i tre sistemi e crea la release.

## Script da terminale (Python)

### Setup (una volta)

1. Vai su <https://my.telegram.org> → **API development tools** e crea un'app: ottieni `api_id` e `api_hash`.
2. Installa:
   ```bash
   python3 -m venv .venv && source .venv/bin/activate && pip install -r requirements.txt
   ```
3. Imposta le credenziali:
   ```bash
   export TG_API_ID=123456 TG_API_HASH=abcdef...
   ```

### Uso

```bash
python download.py --list
```
Al primo avvio chiede numero di telefono e codice di Telegram (la sessione resta salvata in `session.session`: **non condividerlo**, dà accesso al tuo account). Mostra i tuoi gruppi con l'ID.

```bash
python download.py -1001234567890 downloads
```
Al posto dell'ID puoi usare `@username` o un link `t.me/...`. I file finiscono in `downloads/` come `<id messaggio>_<nome>`. Se lo interrompi, rilancialo: salta i file già scaricati.

## Licenza e crediti

Codice sotto licenza [MIT](LICENSE): puoi usarlo, modificarlo e ridistribuirlo liberamente.

- Interfaccia fatta con [Rune](https://github.com/anomalyco/rune), nello stile "poster" di [Spotseek](https://github.com/lopezlav/spotseek).
- Protocollo Telegram: [grammers](https://codeberg.org/Lonami/grammers) (app) e [Telethon](https://github.com/LonamiWebs/Telethon) (script).
- Caratteri [Anton](gui/assets/Anton-LICENSE.txt) e [IBM Plex Mono](gui/assets/IBMPlexMono-LICENSE.txt), licenza SIL Open Font.

Non è un prodotto ufficiale di Telegram. Scarica solo contenuti che hai il diritto di salvare.
