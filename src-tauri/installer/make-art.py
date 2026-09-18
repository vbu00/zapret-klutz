"""Картинки установщика Klutz (NSIS): слева на экранах «Добро пожаловать» и
«Готово» (164x314) и шапка остальных экранов (150x57).

NSIS понимает только BMP, поэтому картинки лежат в репозитории готовыми, а
этот скрипт — чтобы перерисовать их, если поменяется логотип:

    python src-tauri/installer/make-art.py [папка-для-эскизов]

Рисуем в 4 раза крупнее и уменьшаем — так надписи и края логотипа чистые.
Шрифт — Segoe UI из Windows: Manrope в проекте лежит в woff2, Pillow его не
читает. Нужен Pillow.
"""

import os
import sys

from PIL import Image, ImageDraw, ImageFilter, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
ASSETS = os.path.join(ROOT, "src", "assets")
FONTS = os.path.join(os.environ.get("WINDIR", r"C:\Windows"), "Fonts")

S = 4  # во сколько раз крупнее рисуем

GREEN = (18, 160, 92)
INK = (17, 17, 19)
PAPER = (244, 244, 242)
MUTED = (158, 158, 164)


def font(name, size, scale=S):
    return ImageFont.truetype(os.path.join(FONTS, name), size * scale)


def logo(variant, px):
    img = Image.open(os.path.join(ASSETS, f"logo-for-{variant}-256.png")).convert("RGBA")
    return img.resize((px, px), Image.LANCZOS)


def centered(d, y, text, f, fill, width):
    w = d.textlength(text, font=f)
    d.text(((width - w) / 2, y), text, font=f, fill=fill)


def sidebar_big():
    """Картинка слева — в S раз крупнее нужной."""
    W, H = 164 * S, 314 * S
    img = Image.new("RGB", (W, H))
    d = ImageDraw.Draw(img)
    top, bottom = (26, 26, 31), (11, 11, 14)
    for y in range(H):
        t = y / (H - 1)
        d.line([(0, y), (W, y)], fill=tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3)))

    # Мягкое зелёное свечение за логотипом.
    cx, cy = W // 2, 112 * S
    mask = Image.new("L", (W, H), 0)
    r = 62 * S
    ImageDraw.Draw(mask).ellipse([cx - r, cy - r, cx + r, cy + r], fill=70)
    mask = mask.filter(ImageFilter.GaussianBlur(38 * S))
    img = Image.composite(Image.new("RGB", (W, H), GREEN), img, mask)

    mark = logo("dark", 88 * S)
    img.paste(mark, (cx - mark.width // 2, cy - mark.height // 2), mark)

    d = ImageDraw.Draw(img)
    centered(d, 168 * S, "Klutz", font("segoeuib.ttf", 27), PAPER, W)
    f = font("segoeui.ttf", 11)
    centered(d, 210 * S, "Обход блокировок", f, MUTED, W)
    centered(d, 226 * S, "Discord · YouTube · игры", f, MUTED, W)
    d.rounded_rectangle([W // 2 - 14 * S, 284 * S, W // 2 + 14 * S, 287 * S], radius=2 * S, fill=GREEN)
    return img


def header_big():
    """Шапка — белая, как сама шапка установщика."""
    W, H = 150 * S, 57 * S
    img = Image.new("RGB", (W, H), (255, 255, 255))
    mark = logo("light", 34 * S)
    img.paste(mark, (10 * S, (H - mark.height) // 2), mark)
    d = ImageDraw.Draw(img)
    f = font("segoeuib.ttf", 18)
    box = d.textbbox((0, 0), "Klutz", font=f)
    d.text((50 * S, (H - (box[3] - box[1])) / 2 - box[1]), "Klutz", font=f, fill=INK)
    return img


def mock(sidebar, path):
    """Эскиз окна «Добро пожаловать» в 2x — только чтобы посмотреть."""
    k = 2
    W, H = 497 * k, 360 * k
    img = Image.new("RGB", (W, H), (255, 255, 255))
    img.paste(sidebar.resize((164 * k, 314 * k), Image.LANCZOS), (0, 0))
    d = ImageDraw.Draw(img)
    x = 176 * k
    d.text((x, 16 * k), "Вас приветствует мастер", font=font("segoeuib.ttf", 12, k), fill=INK)
    d.text((x, 34 * k), "установки Klutz", font=font("segoeuib.ttf", 12, k), fill=INK)
    body = [
        "Программа установит Klutz на ваш компьютер.",
        "",
        "Перед началом установки рекомендуется закрыть",
        "все работающие приложения.",
        "",
        "Нажмите кнопку «Далее» для продолжения.",
    ]
    f = font("segoeui.ttf", 9, k)
    for i, line in enumerate(body):
        d.text((x, (66 + i * 15) * k), line, font=f, fill=INK)
    d.rectangle([0, 314 * k, W, H], fill=(240, 240, 240))
    d.line([(0, 314 * k), (W, 314 * k)], fill=(210, 210, 210), width=k)
    for label, bx in (("Далее >", 330), ("Отмена", 412)):
        d.rectangle([bx * k, 328 * k, (bx + 75) * k, 351 * k], fill=(253, 253, 253), outline=(173, 173, 173), width=k)
        w = d.textlength(label, font=f)
        d.text((bx * k + (75 * k - w) / 2, 333 * k), label, font=f, fill=INK)
    img.save(path)


def main():
    side = sidebar_big()
    head = header_big()
    side.resize((164, 314), Image.LANCZOS).save(os.path.join(HERE, "sidebar.bmp"))
    head.resize((150, 57), Image.LANCZOS).save(os.path.join(HERE, "header.bmp"))
    if len(sys.argv) > 1:
        out = sys.argv[1]
        os.makedirs(out, exist_ok=True)
        side.resize((164 * 2, 314 * 2), Image.LANCZOS).save(os.path.join(out, "sidebar@2x.png"))
        head.resize((150 * 2, 57 * 2), Image.LANCZOS).save(os.path.join(out, "header@2x.png"))
        mock(side, os.path.join(out, "welcome-mock.png"))
    print("готово:", os.path.join(HERE, "sidebar.bmp"), os.path.join(HERE, "header.bmp"))


if __name__ == "__main__":
    main()
