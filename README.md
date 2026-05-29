# Timluli — תמלול קולי עברית לווינדוס

> כלי תמלול קולי system-wide לווינדוס, עם תמיכה בעברית ובאנגלית.

<!-- TODO: סקרינשוט של המיקרופון הצף וחלון ההגדרות -->

**Timluli** הוא אפליקציית Tauri שמאפשרת תמלול קולי **בכל אפליקציה ובכל שדה טקסט בווינדוס** — מהטרמינל ועד Word, מ-VS Code ועד WhatsApp Web. הטקסט מוזרק כקלט מקלדת אמיתי באמצעות `SendInput`.

האפליקציה תומכת בשני מנועי תמלול:
- **Web Speech (מקוון)** — Google Web Speech API דרך WebView2, ללא התקנה נוספת
- **Whisper Local (אופליין)** — מנוע Whisper.cpp מקומי עם מודל עברית של [ivrit-ai](https://huggingface.co/ivrit-ai), ללא שליחת אודיו לשרת

בנוסף, Timluli כולל **תרגום מסמכים** — גרור קובץ כתוביות, טקסט או מסמך (DOCX/DOC/PDF) על אייקון המיקרופון, והעותק המתורגם יישמר באותה תיקייה. התרגום מתבצע דרך Groq/Cerebras (מנועי ענן תואמי-OpenAI) עם שרשרת fallback אוטומטית ופלט RTL לעברית. **תרגום PDF לעברית שומר על הפריסה המקורית** — מיקום הטקסט, טבלאות, לוגואים ומשוואות — ומפיק PDF עברי שנראה כמו המקור.

---

## תכונות

- 🎤 מיקרופון מרחף `topmost` עם `WS_EX_NOACTIVATE` — לא גונב פוקוס, גרירה ושמירת מיקום
- ⌨️ קיצור מקלדת גלובלי (ברירת מחדל: `Ctrl+Win+Space`)
- 🔄 מצב Toggle (לחץ להתחיל / לחץ לסיים) — Push-to-Talk קיים בהגדרות אך טרם ממומש
- 🔊 תמיכה בעברית (`he-IL`) ובאנגלית (`en-US`)
- 📋 הזרקת טקסט יוניקוד מלאה דרך `SendInput` עם Clipboard fallback לטקסטים ארוכים
- 🌐 מנוע מקוון: Google Web Speech API (ללא התקנה)
- 💾 מנוע אופליין: Whisper Local — הורדה חד-פעמית (~1.5 GB), עובד ללא אינטרנט
- 🎨 ערכות עיצוב למיקרופון (graphite, כחול, אדום, ירוק, שקיעה, אוקיינוס, סגול, פלזמה, זוהר הצפון)
- 📄 תרגום מסמכים בגרירה — כתוביות (SRT, VTT, SBV), טקסט (TXT, MD), ומסמכים (DOCX, DOC, PDF) דרך Groq/Cerebras, עם שמירת חותמות זמן ומבנה ופלט RTL לעברית
- 🧩 תרגום PDF→PDF לעברית עם **שימור פריסה מלא** (טקסט, טבלאות, לוגואים, משוואות) — דרך מנוע PyMuPDF נלווה
- 🔑 אחסון מאובטח של מפתחות API — מוצפנים at-rest ב-Windows DPAPI
- 🪟 System tray עם תפריט קליק ימני
- ⚙️ חלון הגדרות עם 6 טאבים (כללי, קיצור מקלדת, תצוגה, התנהגות, מנוע תמלול, תרגום מסמכים)
- 🚀 אפשרות להפעלה אוטומטית עם Windows
- 🧙 אשף Onboarding בהפעלה ראשונה

---

## פרטיות ונתונים

### מנוע Web Speech (מקוון)

Timluli משתמש ב-Web Speech API המובנה ב-WebView2 (מנוע Chromium של Microsoft Edge) לזיהוי דיבור.

- **האודיו נשלח לשירות חיצוני** — Web Speech API ב-WebView2 מעביר את האודיו לשירות Google Speech. נדרש חיבור אינטרנט פעיל.
- **Timluli עצמה אינה שומרת אודיו, תמלולים, או היסטוריה** — לא בדיסק המקומי ולא בענן.
- **קובץ ההגדרות** (`%APPDATA%\studio.oliel.timluli\settings.json`) מכיל אך ורק העדפות משתמש.
- **מדיניות הפרטיות של Google** חלה על האודיו שנשלח, ואינה בשליטת Timluli.

### מנוע Whisper Local (אופליין)

- **האודיו לא נשלח לשום שרת** — כל העיבוד מתבצע על המחשב המקומי.
- נדרשת הורדה חד-פעמית של המודל (~1.5 GB) בחיבור אינטרנט.

### תרגום מסמכים

- **תוכן הקובץ נשלח לספק שבחרת** — Groq או Cerebras (endpoints תואמי-OpenAI). רק הטקסט לתרגום נשלח; מבנה הקובץ (מספרי כתוביות, חותמות זמן, פריסת פסקאות וטבלאות) מעובד מקומית ולא נשלח.
- **המפתחות נשמרים מוצפנים** ב-`secrets.json` (Windows DPAPI, קשור למשתמש Windows) — לעולם לא בטקסט גלוי, ולא נשלחים לאף שרת מלבד הספק.
- הקריאות יוצאות מצד ה-Rust (לא מה-webview), ולכן אינן כפופות ל-CSP של האפליקציה.

### סיכום

| מנוע | אודיו לשרת | שמירה מקומית | דורש אינטרנט |
|------|-----------|-------------|--------------|
| Web Speech (Google) | כן | לא | כן |
| Whisper Local | לא | לא | לא (לאחר הורדה) |
| תרגום מסמכים (Groq/Cerebras) | טקסט בלבד | קובץ פלט מקומי | כן |

---

## תרגום מסמכים

גרור קובץ נתמך על אייקון המיקרופון המרחף. Timluli מתרגם אותו ושומר עותק חדש באותה תיקייה, ליד המקור, עם שפת היעד כסיומת — לדוגמה `movie.srt` → `movie.hebrew.srt`. המקור לא משתנה.

**פורמטים נתמכים:** SRT, VTT, SBV (כתוביות) · TXT, MD (טקסט) · DOCX, DOC, PDF (מסמכים). בכתוביות נשמרים המספרים וחותמות הזמן במדויק; ב-Markdown בלוקי קוד (```` ``` ````) לא מתורגמים. ב-DOCX נשמר מבנה המסמך (פסקאות, טבלאות) והפלט מסומן RTL לעברית.

**תרגום PDF:**
- **יעד עברית** → הפלט הוא **PDF עם שימור פריסה מלא**: הטקסט המקורי מוסר ומוחלף בעברית במקומו, תוך שמירה על טבלאות, לוגואים, איורים ומשוואות (משוואות נשארות כפי שהן). מתבצע דרך מנוע `timluli-pdf` נלווה (PyMuPDF + python-bidi) הנארז עם האפליקציה.
- **יעד שאינו עברית** → הפלט הוא **DOCX** (שחזור פריסת PDF נכון רק לעברית RTL).

תרגום DOC דורש LibreOffice מותקן.

**הגדרה (טאב "תרגום מסמכים"):**
1. בחר שפת יעד (ברירת מחדל: עברית).
2. הזן מפתח API — לפחות אחד מהשניים:
   - **Groq** (מומלץ, מהיר וחינמי) — [console.groq.com/keys](https://console.groq.com/keys)
   - **Cerebras** (גיבוי) — [cloud.cerebras.ai](https://cloud.cerebras.ai/)
3. שמור. המפתחות נשמרים מוצפנים (DPAPI) ומופיע "מפתח שמור ✓".

**Fallback ועמידות ל-rate-limit:** המנוע עובר בין מספר מודלים של Groq ואז Cerebras. כשמכסה קבועה מסתיימת (402) הוא ממשיך אוטומטית למודל הבא; כשנתקל ב-rate-limit זמני (429) הוא ממתין לפי `Retry-After` ומנסה שוב את אותו מודל. גודל הבקשה מותאם כדי להישאר מתחת למגבלות ה-TPM של ה-tier החינמי, כך שתרגום ארוך לא נעצר.

---

## הצהרת אחריות

Timluli עושה שימוש ב-Web Speech API של הדפדפן באמצעות WebView2. השימוש בשירותי זיהוי הדיבור הבסיסיים (Google Speech Services) כפוף לתנאי השימוש של אותם ספקים, והאחריות לעמידה בתנאים אלה היא על המשתמש הקצה. המפתחים אינם אחראים לזמינות, איכות, דיוק או חוקיות השימוש בשירותי הזיהוי החיצוניים בשיפוטים השונים.

הקוד של Timluli מופץ תחת רישיון MIT — ראה [LICENSE](LICENSE).

---

## דרישות מערכת

- **מערכת הפעלה:** Windows 10/11 (64-bit)
- **WebView2 Runtime** — מובנה ב-Windows 11; ב-Windows 10 מותקן אוטומטית עם ה-installer
- **חיבור אינטרנט** — נדרש למנוע Web Speech; מנוע Whisper Local עובד אופליין לאחר הורדת המודל
- **מיקרופון** מחובר ופועל

---

## התקנה

הורד את הגרסה האחרונה מעמוד [Releases ב-GitHub](https://github.com/ddlgap/Timluli/releases) (**NSIS installer** מומלץ).

לאחר ההתקנה, Timluli מופעל עם ה-system tray. לחץ `Ctrl+Win+Space` כדי להתחיל תמלול.

---

## ארכיטקטורה

Timluli בנוי כתהליך Tauri יחיד (Rust) עם ארבעה חלונות WebView2:

- **`mic`** — מיקרופון מרחף, frameless, transparent, תמיד מעל (NOACTIVATE — לא גונב פוקוס)
- **`speech`** — חלון נסתר המריץ את `webkitSpeechRecognition` (מנוע Google) ברקע
- **`settings`** — חלון הגדרות עם 6 טאבים, נפתח לפי דרישה
- **`onboarding`** — אשף הגדרה ראשוני, מוצג בהפעלה הראשונה

תקשורת פנימית: ~35 Tauri commands ו-10 events בין ה-frontend (JS) ל-backend (Rust). שכבת Win32 משתמשת ב-`SendInput`, `AttachThreadInput`, `SetForegroundWindow`, ו-`WS_EX_NOACTIVATE` לניהול פוקוס והזרקת טקסט, וב-DPAPI (`CryptProtectData`) לאחסון מוצפן של מפתחות התרגום. מנוע התרגום (`translation/`) מבצע את הקריאות ל-Groq/Cerebras דרך `reqwest`.

לתיעוד טכני מלא ראה [BLUEPRINT.md](./docs/BLUEPRINT.md).

### זרימה

```
[קיצור מקלדת / לחיצה על מיקרופון]
              ↓
   Rust: capture_target_window() — שמירת HWND
              ↓
     engine_id == "web-speech"?
    ┌──────────┴───────────┐
   כן                      לא (whisper-local)
    ↓                       ↓
[Hidden WebView2]       [הקלטת אודיו]
webkitSpeechRecognition  whisper-rs (inference מקומי)
→ Google STT             ↓
    └──────────┬──────────┘
               ↓
   Rust: inject_text() — SetForegroundWindow + SendInput
               ↓
   [חלון יעד] ← טקסט מוזרק
```

---

## בנייה מהמקור

### דרישות מקדימות

| כלי | גרסה מינימלית | הערה |
|-----|--------------|------|
| Rust (MSVC target) | 1.77 | `rustup install stable` |
| Node.js | 18+ | |
| Visual Studio Build Tools | 2019/2022 | C++ workload |
| CMake | 3.x | נדרש ל-whisper-rs |
| LLVM/Clang | כל גרסה עדכנית | נדרש ל-bindgen |
| Python | 3.12 | נדרש לבניית מנוע ה-PDF הנלווה (PyInstaller) |

לפרטי התקנה מלאים ראה [BUILDING.md](BUILDING.md).

### שלבי בנייה

```powershell
npm install
src-tauri\sidecar\build_sidecar.ps1   # בונה את מנוע ה-PDF הנלווה (פעם אחת, ולאחר עריכת timluli_pdf.py)
npm run tauri:dev                      # סביבת פיתוח עם hot-reload
npm run tauri:build                    # בנייה ל-production
```

> מנוע ה-PDF (`timluli-pdf.exe`) חייב להיבנות **לפני** `tauri:dev`/`tauri:build`, כיוון ש-Tauri אורז את תיקיית `resources/` בזמן הקומפילציה.

**פלט הבינארי:** `src-tauri/target/release/bundle/` — MSI ו-NSIS installer.

---

## באגים ידועים

| באג | תיאור |
|-----|-------|
| **Push-to-Talk לא פעיל** | מצב "לחץ-והחזק" קיים בהגדרות אך עדיין לא ממומש. מצב Toggle עובד כרגיל. |
| **הזרקה לחלון הלא נכון** | אם תעבור לחלון אחר *במהלך* התמלול, הטקסט יוזרק לחלון המקורי שנלכד בתחילת ההקלטה. |
| **דריסת לוח הגזרים** | טקסטים ארוכים מוזרקים דרך Clipboard ואינם משחזרים את תוכנו הקודם. |
| **כשלי הזרקה שקטים** | אם תהליך היעד מורץ כ-Administrator ו-Timluli אינו, ה-UIPI חוסם את `SendInput` ללא הודעת שגיאה. |
| **שגיאות רשת/הרשאות אחידות** | שגיאת אינטרנט ושגיאת מיקרופון מציגות אותה הודעה גנרית. |

---

## Roadmap

- ✅ **תמלול מקומי (offline)** — ממומש עם whisper.cpp + whisper-rs + מודל ivrit-ai
- ✅ **תרגום מסמכים** — גרירת כתוביות/טקסט/DOCX/DOC/PDF על המיקרופון, תרגום דרך Groq/Cerebras עם fallback ופלט RTL לעברית
- ✅ **שימור פריסת PDF** — תרגום PDF→PDF לעברית עם שימור פריסה מלא (טקסט, טבלאות, לוגואים, משוואות) דרך מנוע PyMuPDF נלווה
- 🔲 **Voice Activity Detection (VAD)** — זיהוי דיבור מקומי (silero-vad) לשיפור חיסכון סוללה ודיוק
- 🔲 **Push-to-Talk** — מימוש מצב "לחץ-והחזק"
- 🔲 **גיבוי ושחזור לוח הגזרים** — שמירה ושחזור תוכן ה-Clipboard סביב פעולות הזרקה
- 🔲 **ולידציית HWND** — בדיקה שחלון היעד עדיין קיים ופעיל לפני הזרקה
- 🔲 **השתקה אוטומטית** — במצב מסך מלא או שיחות וידאו
- 🔲 **שיפור הודעות שגיאה** — הבחנה בין שגיאות רשת, מיקרופון, ו-UIPI

---

## תרומה לפרויקט

פתח Issue או PR ב-GitHub. אין מדיניות תרומה פורמלית בשלב זה.

## אזכורים

- [Tauri](https://tauri.app) — מסגרת הפיתוח
- [ivrit-ai](https://huggingface.co/ivrit-ai) — מודל Whisper לעברית
- [whisper.cpp](https://github.com/ggerganov/whisper.cpp) — מנוע inference מקומי
- [whisper-rs](https://github.com/tazz4843/whisper-rs) — ממשק Rust ל-whisper.cpp
- [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/) — Google Web Speech API
- [Groq](https://groq.com) · [Cerebras](https://cerebras.ai) — מנועי תרגום מסמכים (endpoints תואמי-OpenAI)

---

## קרדיט

Daniel Oliel / Oliel Studio · 2026

## License

MIT License — see [LICENSE](LICENSE) for details.
