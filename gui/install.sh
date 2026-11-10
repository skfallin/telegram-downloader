#!/bin/sh
# Compila l'app e la mette in /Applications come "Telegram Downloader". Rilancialo dopo ogni modifica.
# Uso: ./install.sh [cartella]  (BIN=percorso usa un eseguibile già compilato, come fa la release)
set -e
cd "$(dirname "$0")"
if [ -z "$BIN" ]; then cargo build --release; BIN=target/release/telegram-downloader; fi
APP="${1:-/Applications}/Telegram Downloader.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/telegram-downloader"
mkdir -p "$APP/Contents/Resources"
cp assets/icon.icns "$APP/Contents/Resources/icon.icns"
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Telegram Downloader</string>
  <key>CFBundleDisplayName</key><string>Telegram Downloader</string>
  <key>CFBundleIdentifier</key><string>local.telegram-downloader</string>
  <key>CFBundleExecutable</key><string>telegram-downloader</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundleVersion</key><string>1.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF
echo "Installata: $APP"
