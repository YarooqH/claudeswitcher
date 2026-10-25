"""Builds assets/icon.ico from logo-dithered.jpg.

Run from the repository root:  python tools/make_icon.py
Needs Pillow. The icon is the logo on a rounded dark tile, at every size Windows asks for.
"""
from PIL import Image, ImageDraw

SOURCE = "logo-dithered.jpg"
TARGET = "assets/icon.ico"
SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
# Bounding box of the artwork inside the 2048 px source, and the room left around it.
ARTWORK = (329, 559, 1717, 1488)
MARGIN = 110
CORNER = 0.22


def tile(source: Image.Image) -> Image.Image:
    left, top, right, bottom = ARTWORK
    centre_x, centre_y = (left + right) / 2, (top + bottom) / 2
    half = (max(right - left, bottom - top) + 2 * MARGIN) / 2
    square = source.crop(
        (round(centre_x - half), round(centre_y - half), round(centre_x + half), round(centre_y + half))
    ).convert("RGBA")

    # Rounded corners, drawn at 4x and shrunk so the edge is smooth.
    side = square.width
    mask = Image.new("L", (side * 4, side * 4), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, side * 4 - 1, side * 4 - 1), radius=side * 4 * CORNER, fill=255)
    square.putalpha(mask.resize((side, side), Image.LANCZOS))
    return square


def main() -> None:
    master = tile(Image.open(SOURCE))
    # Shrink in two steps: straight from 1600 px down to 16 px aliases the dots.
    base = master.resize((512, 512), Image.LANCZOS)
    frames = [base.resize((size, size), Image.LANCZOS) for size in SIZES]
    frames[-1].save(TARGET, format="ICO", sizes=[(size, size) for size in SIZES], append_images=frames[:-1])
    print(f"wrote {TARGET} ({', '.join(str(size) for size in SIZES)})")


if __name__ == "__main__":
    main()
