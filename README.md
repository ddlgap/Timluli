# Timluli — תמלול קולי עברית לווינדוס

> כלי תמלול קולי system-wide לווינדוס, עם תמיכה בעברית ובאנגלית.

<!-- TODO: סקרינשוט של המיקרופון הצף וחלון ההגדרות -->

**Timluli** הוא אפליקציית Tauri שמאפשרת תמלול קולי **בכל אפליקציה ובכל שדה טקסט בווינדוס** — מהטרמינל ועד Word, מ-VS Code ועד WhatsApp Web. הטקסט מוזרק כקלט מקלדת אמיתי באמצעות `SendInput`.

האפליקציה תומכת בשני מנועי תמלול:
- **Web Speech (מקוון)** — Google Web Speech API דרך WebView2, ללא התקנה נוספת
- **Whisper Local (אופליין)** — מנוע Whisper.cpp מקומי עם מודל עברית של [ivrit-ai](https://huggingface.co/ivrit-ai), ללא שליחת אודיו לשרת

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
- 🪟 System tray עם תפריט קליק ימני
- ⚙️ חלון הגדרות עם 5 טאבים (כללי, קיצור מקלדת, תצוגה, התנהגות, מנוע תמלול)
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

### סיכום

| מנוע | אודיו לשרת | שמירה מקומית | דורש אינטרנט |
|------|-----------|-------------|--------------|
| Web Speech (Google) | כן | לא | כן |
| Whisper Local | לא | לא | לא (לאחר הורדה) |

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

הורד את הגרסה האחרונה מעמוד [Releases ב-GitHub](<!-- TODO: קישור לעמוד Releases -->) (**NSIS installer** מומלץ).

לאחר ההתקנה, Timluli מופעל עם ה-system tray. לחץ `Ctrl+Win+Space` כדי להתחיל תמלול.

---

## ארכיטקטורה

Timluli בנוי כתהליך Tauri יחיד (Rust) עם ארבעה חלונות WebView2:

- **`mic`** — מיקרופון מרחף, frameless, transparent, תמיד מעל (NOACTIVATE — לא גונב פוקוס)
- **`speech`** — חלון נסתר המריץ את `webkitSpeechRecognition` (מנוע Google) ברקע
- **`settings`** — חלון הגדרות עם 5 טאבים, נפתח לפי דרישה
- **`onboarding`** — אשף הגדרה ראשוני, מוצג בהפעלה הראשונה

תקשורת פנימית: ~31 Tauri commands ו-7 events בין ה-frontend (JS) ל-backend (Rust). שכבת Win32 משתמשת ב-`SendInput`, `AttachThreadInput`, `SetForegroundWindow`, ו-`WS_EX_NOACTIVATE` לניהול פוקוס והזרקת טקסט.

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

לפרטי התקנה מלאים ראה [BUILDING.md](BUILDING.md).

### שלבי בנייה

```bash
npm install
npm run tauri:dev     # סביבת פיתוח עם hot-reload
npm run tauri:build   # בנייה ל-production
```

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

---

## קרדיט

Daniel Oliel / Oliel Studio · 2026

## License

MIT License — see [LICENSE](LICENSE) for details.
