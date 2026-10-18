"""Скриншоты для README: снимает макет интерфейса (ui/index.html#still&...) headless Edge
и оформляет кадры — скруглённые углы, рамка, мягкая тень, прозрачный фон.

    python scripts/make-screenshots.py
"""
import os
import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "screenshots"
EDGE = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
UI = (ROOT / "ui" / "index.html").as_uri()
SCALE = 2
SHOTS = {
    "idle": "dark",
    "run": "dark&run",
    "queue": "dark&queue",
    "done": "light&done",
    "models": "dark&models&dl",
    "archive": "light&archive",
    "settings": "dark&settings",
}


def capture(name: str, flags: str, folder: Path) -> Path:
    path = folder / f"{name}.png"
    subprocess.run(
        [EDGE, "--headless=new", "--disable-gpu", "--hide-scrollbars", f"--force-device-scale-factor={SCALE}",
         "--window-size=1100,720", "--virtual-time-budget=3000", f"--screenshot={path}", f"{UI}#still&{flags}"],
        check=True, capture_output=True,
    )
    return path


def frame(img: Image.Image, dark: bool) -> Image.Image:
    """Окно с тенью на прозрачном фоне."""
    r = 12 * SCALE
    w, h = img.size
    mask = Image.new("L", img.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, w - 1, h - 1], radius=r, fill=255)
    window = img.convert("RGBA")
    window.putalpha(mask)
    border = (255, 255, 255, 34) if dark else (0, 0, 0, 40)
    ImageDraw.Draw(window).rounded_rectangle([0, 0, w - 1, h - 1], radius=r, outline=border, width=SCALE)

    pad = 48 * SCALE
    canvas = Image.new("RGBA", (w + pad * 2, h + pad * 2), (0, 0, 0, 0))
    shadow = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle([pad, pad + 14 * SCALE, pad + w, pad + h + 14 * SCALE], radius=r, fill=(0, 0, 0, 110))
    shadow = shadow.filter(ImageFilter.GaussianBlur(22 * SCALE))
    canvas.alpha_composite(shadow)
    canvas.alpha_composite(window, (pad, pad))
    return canvas


def save(img: Image.Image, name: str, width: int = 1600) -> None:
    if img.width > width:
        img = img.resize((width, round(img.height * width / img.width)), Image.LANCZOS)
    img.save(OUT / f"{name}.png", optimize=True)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        raw = {name: Image.open(capture(name, flags, Path(tmp))).copy() for name, flags in SHOTS.items()}
    for name, img in raw.items():
        save(frame(img, dark="light" not in SHOTS[name]), name)

    # Шапка README: тёмное окно впереди, светлое — позади со сдвигом.
    back = frame(raw["done"], dark=False)
    front = frame(raw["run"], dark=True)
    dx, dy = 340 * SCALE, 150 * SCALE
    hero = Image.new("RGBA", (back.width + dx, back.height + dy), (0, 0, 0, 0))
    hero.alpha_composite(back, (dx, 0))
    hero.alpha_composite(front, (0, dy))
    save(hero, "hero", width=1800)
    print("готово:", ", ".join(sorted(p.name for p in OUT.glob("*.png"))))


if __name__ == "__main__":
    main()
