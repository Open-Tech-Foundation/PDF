#!/usr/bin/env python3
"""Generate the Indic and CJK text-extraction fixtures in corpus/text.

Short original texts in Hindi, Bengali, Tamil, Telugu, Chinese, Japanese and
Korean are printed to PDF by two independent producers, Chromium (Skia) and
LibreOffice, with the system's Noto/WenQuanYi/VL fonts. The source text is
the ground truth the text corpus test measures extraction against, which is
stricter than comparing with another extractor. The PDFs are committed;
regenerating needs chromium, soffice and those fonts, and the bytes vary with
their versions.

    scripts/generate-script-fixtures.py
"""

import shutil
import subprocess
import tempfile
from pathlib import Path

root = Path(__file__).resolve().parent.parent
out_dir = root / "corpus" / "text"

TEXTS = {
    "hindi": ("indic", "hi", "Noto Serif Devanagari",
              "भारत एक विशाल देश है। यहाँ अनेक भाषाएँ बोली जाती हैं।\n"
              "हम हर दिन नई बातें सीखते हैं और अपने मित्रों के साथ समय बिताते हैं।\n"
              "पुस्तकालय में बहुत सारी किताबें रखी हुई हैं जिन्हें विद्यार्थी पढ़ते हैं।"),
    "bengali": ("indic", "bn", "Noto Sans Bengali",
                "আমাদের গ্রামে একটি ছোট নদী আছে। সকালে পাখিরা গান গায়।\n"
                "শিশুরা মাঠে খেলা করে এবং সন্ধ্যায় বাড়ি ফিরে আসে।"),
    "tamil": ("indic", "ta", "Noto Serif Tamil Slanted",
              "தமிழ் மிகவும் பழமையான மொழிகளில் ஒன்று. இங்கு பல கவிஞர்கள் வாழ்ந்தனர்.\n"
              "மாணவர்கள் தினமும் பள்ளிக்குச் சென்று புதிய பாடங்களைக் கற்கிறார்கள்."),
    "telugu": ("indic", "te", "Noto Sans Telugu",
               "తెలుగు ఒక అందమైన భాష. మా ఊరిలో పెద్ద చెరువు ఉంది.\n"
               "పిల్లలు ఉదయం బడికి వెళ్లి సాయంత్రం ఇంటికి తిరిగి వస్తారు."),
    "chinese": ("cjk", "zh", "WenQuanYi Micro Hei",
                "今天天气很好，我们一起去公园散步。\n"
                "图书馆里有很多书，学生们每天都在这里学习。\n"
                "春天来了，花园里开满了美丽的花。"),
    "japanese": ("cjk", "ja", "VL Gothic",
                 "今日はとても良い天気です。私たちは公園を散歩しました。\n"
                 "図書館にはたくさんの本があり、学生は毎日ここで勉強します。"),
    "korean": ("cjk", "ko", "WenQuanYi Micro Hei",
               "오늘은 날씨가 아주 좋습니다. 우리는 공원에서 산책을 했습니다.\n"
               "도서관에는 책이 많이 있고 학생들은 매일 여기서 공부합니다."),
}


def html(lang, font, text):
    paragraphs = "".join(f"<p>{line}</p>" for line in text.splitlines())
    return (f'<!doctype html><html lang="{lang}"><meta charset="utf-8">'
            f'<style>body{{font-family:"{font}";font-size:16pt;margin:2cm}}</style>{paragraphs}</html>')


def main():
    chromium = shutil.which("chromium") or shutil.which("google-chrome")
    soffice = shutil.which("soffice")
    if not chromium or not soffice:
        raise SystemExit("needs chromium and soffice")
    out_dir.mkdir(parents=True, exist_ok=True)
    rows = ["# file\tgroup\tsource text"]
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for name, (group, lang, font, text) in TEXTS.items():
            src = tmp / f"{name}.html"
            src.write_text(html(lang, font, text), encoding="utf-8")
            (out_dir / f"{name}.txt").write_text(text + "\n", encoding="utf-8")
            target = out_dir / f"{name}-chromium.pdf"
            subprocess.run([chromium, "--headless", "--disable-gpu", "--no-pdf-header-footer",
                            f"--print-to-pdf={target}", src.as_uri()], check=True, capture_output=True)
            subprocess.run([soffice, "--headless", "--convert-to", "pdf", "--outdir", str(tmp), str(src)],
                           check=True, capture_output=True)
            shutil.copy(tmp / f"{name}.pdf", out_dir / f"{name}-libreoffice.pdf")
            for producer in ("chromium", "libreoffice"):
                rows.append(f"{name}-{producer}.pdf\t{group}\t{name}.txt")
    (out_dir / "manifest.tsv").write_text("\n".join(rows) + "\n", encoding="utf-8")
    print(f"wrote {len(TEXTS) * 2} PDFs to {out_dir.relative_to(root)}")


if __name__ == "__main__":
    main()
