"""Scarica tutti i file (foto, video, documenti, audio...) da un gruppo/canale Telegram.

Uso:
  python download.py --list                 # mostra i tuoi gruppi con il loro ID
  python download.py <gruppo> [cartella]    # gruppo = @username, link t.me o ID numerico
"""
import asyncio
import os
import sys

from telethon import TelegramClient


def file_name(msg_id, name, ext):
    # basename: il nome arriva da Telegram, non deve poter uscire dalla cartella
    base = os.path.basename(name or "") or f"file{ext or ''}"
    return f"{msg_id}_{base}"


async def main():
    args = sys.argv[1:]
    if not args:
        sys.exit(__doc__)
    api_id, api_hash = os.environ.get("TG_API_ID"), os.environ.get("TG_API_HASH")
    if not api_id or not api_hash:
        sys.exit("Imposta TG_API_ID e TG_API_HASH (vedi README).")

    # flood_sleep_threshold alto: se Telegram rallenta, aspetta invece di fallire
    client = TelegramClient("session", int(api_id), api_hash, flood_sleep_threshold=24 * 3600)
    async with client:
        if args[0] == "--list":
            async for d in client.iter_dialogs():
                if d.is_group or d.is_channel:
                    print(f"{d.id}\t{d.name}")
            return

        group = int(args[0]) if args[0].lstrip("-").isdigit() else args[0]
        out_dir = args[1] if len(args) > 1 else "downloads"
        os.makedirs(out_dir, exist_ok=True)

        count = 0
        async for msg in client.iter_messages(group):
            if not msg.file:
                continue
            path = os.path.join(out_dir, file_name(msg.id, msg.file.name, msg.file.ext))
            if os.path.exists(path) and os.path.getsize(path) == msg.file.size:
                continue  # già scaricato: si può rilanciare per riprendere
            print(f"↓ {path} ({(msg.file.size or 0) / 1e6:.1f} MB)")
            tmp = path + ".part"  # niente file mezzi scaricati se si interrompe
            await msg.download_media(file=tmp)
            os.replace(tmp, path)
            count += 1
        print(f"Fatto: {count} nuovi file in {out_dir}/")


if __name__ == "__main__":
    asyncio.run(main())
