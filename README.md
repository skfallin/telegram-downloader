# Telegram downloader

Scarica tutti i file di un gruppo o canale Telegram di cui fai parte, usando il tuo account.

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

Serve [Rust](https://rustup.rs). Su Linux anche `libwayland-dev libxkbcommon-dev libgl1-mesa-dev`.

```bash
cd gui && cargo run --release
```
Su macOS `gui/install.sh` la compila e la mette in Applicazioni.

Per pubblicare una nuova versione: `git tag v0.2.0 && git push --tags`. GitHub Actions compila per i tre sistemi e crea la release.

## Script da terminale (Python)

## Setup (una volta)

1. Vai su <https://my.telegram.org> → **API development tools** e crea un'app: ottieni `api_id` e `api_hash`.
2. Installa:
   ```bash
   python3 -m venv .venv && source .venv/bin/activate && pip install -r requirements.txt
   ```
3. Imposta le credenziali:
   ```bash
   export TG_API_ID=123456 TG_API_HASH=abcdef...
   ```

## Uso

```bash
python download.py --list
```
Al primo avvio chiede numero di telefono e codice di Telegram (la sessione resta salvata in `session.session`: **non condividerlo**, dà accesso al tuo account). Mostra i tuoi gruppi con l'ID.

```bash
python download.py -1001234567890 downloads
```
Al posto dell'ID puoi usare `@username` o un link `t.me/...`. I file finiscono in `downloads/` come `<id messaggio>_<nome>`. Se lo interrompi, rilancialo: salta i file già scaricati.
