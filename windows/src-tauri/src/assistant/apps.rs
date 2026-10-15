// The apps "open <name>" may start (`launch-app`): Windows' own "All apps"
// list, the one Start shows (the shell's AppsFolder), Store apps included.
// That is the controlled list: what the user could already start from Start,
// under the name Start shows. Never a path, a command line or arguments from
// the page; an app is named by an id this module made from its AppsFolder
// entry, found again from the id when the user confirms, and opened the way
// Start opens it (`shell:AppsFolder\<AppID>`, launch.rs). A Start menu
// shortcut AppsFolder is missing (a broken registration) is taken from the
// Start menu's own folders and opened as that shortcut.
//
// Uninstallers, readmes, help and web links are left out. Matching a spoken
// name is deterministic: exact names before interpreting leading articles or
// app qualifiers, then its first words, then one of its words, then a near
// spelling (Whisper's), the shortest name first on a tie.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// `<slug>-<8 hex>`: the name, and which entry (two "Notepad"s differ).
    pub id: String,
    /// The app's name, as Start shows it.
    pub label: String,
    /// Its AppsFolder AppID: an AUMID (`Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`)
    /// or a path-like id for a desktop app.
    pub target: String,
}

/// Entries that are not an app to open.
const NOT_APPS: [&str; 13] = [
    "uninstall", "désinstaller", "desinstaller", "deinstallieren", "readme", "read me", "help", "aide",
    "documentation", "manual", "license", "release notes", "website",
];

/// Documents and links in the list: opened by another app, not apps.
const NOT_APP_TARGETS: [&str; 12] =
    [".url", ".htm", ".html", ".pdf", ".txt", ".chm", ".rtf", ".doc", ".docx", ".md", ".ini", ".log"];

/// Lowercase words, accents and punctuation gone: "Visual Studio Code" →
/// "visual studio code", "Café-Bar" → "cafe bar".
pub fn normalise(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        let c = fold_accent(c);
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_string()
}

fn fold_accent(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        c => c,
    }
}

/// The entry for one item of the list, unless it isn't an app to open.
pub fn entry_for(label: &str, target: &str) -> Option<AppEntry> {
    let label = label.trim();
    let target = target.trim();
    let lower = label.to_lowercase();
    let t = target.to_lowercase();
    if label.is_empty()
        || t.is_empty()
        || NOT_APPS.iter().any(|w| lower.contains(w))
        || t.contains("://")
        || NOT_APP_TARGETS.iter().any(|ext| t.ends_with(ext))
        || target.chars().any(|c| c.is_control() || c == '"')
    {
        return None;
    }
    Some(AppEntry { id: id_for(label, target), label: label.to_string(), target: target.to_string() })
}

/// The list's items as entries, the first of a name kept.
pub fn entries(items: impl IntoIterator<Item = (String, String)>) -> Vec<AppEntry> {
    let mut out: Vec<AppEntry> = Vec::new();
    for (label, target) in items {
        if let Some(entry) = entry_for(&label, &target) {
            if !out.iter().any(|e| e.label.eq_ignore_ascii_case(&entry.label)) {
                out.push(entry);
            }
        }
    }
    out
}

/// What `policy::app_id_ok` accepts: lowercase ASCII letters and digits with
/// `-`, at most 64 characters.
pub fn id_for(label: &str, target: &str) -> String {
    let mut slug: String = normalise(label)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug = slug.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    slug.truncate(40);
    let slug = slug.trim_end_matches('-');
    let slug = if slug.is_empty() { "app" } else { slug };
    let digest = ring::digest::digest(&ring::digest::SHA256, target.trim().to_lowercase().as_bytes());
    let hex: String = digest.as_ref()[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("{slug}-{hex}")
}

/// French names of Windows' own apps, for a Windows in English (normalised
/// spoken name → the English name Start shows). Only used when the spoken
/// name itself finds nothing.
const FRENCH_NAMES: [(&str, &str); 14] = [
    ("calculatrice", "calculator"),
    ("bloc notes", "notepad"),
    ("parametres", "settings"),
    ("reglages", "settings"),
    ("explorateur de fichiers", "file explorer"),
    ("explorateur", "file explorer"),
    ("horloge", "clock"),
    ("appareil photo", "camera"),
    ("outil capture", "snipping tool"),
    ("gestionnaire des taches", "task manager"),
    ("invite de commandes", "command prompt"),
    ("panneau de configuration", "control panel"),
    ("meteo", "weather"),
    ("enregistreur vocal", "sound recorder"),
];

// Keep in sync with intents.ts. The frontend preserves the original query so
// "The Witness" can beat "Witness", including after "the app" or "une appli".
const OPEN_FILLERS: [&str; 14] = [
    "moi", "l", "la", "le", "les", "un", "une", "des", "app", "application", "the", "a", "an", "appli",
];

/// The app a spoken query means, if any. Exact labels at each leading-filler
/// boundary win longest-first; only the fully stripped tail may match loosely.
pub fn find<'a>(entries: &'a [AppEntry], spoken: &str) -> Option<&'a AppEntry> {
    find_with(entries, spoken, true)
}

/// The app a typed name means (the chat's `open_app`): the same, without
/// near spellings. A model spells "Chrome" right; when Chrome isn't in the
/// list, "Dev Home" is not what it meant.
pub fn find_typed<'a>(entries: &'a [AppEntry], name: &str) -> Option<&'a AppEntry> {
    find_with(entries, name, false)
}

fn find_with<'a>(entries: &'a [AppEntry], spoken: &str, near: bool) -> Option<&'a AppEntry> {
    let q = normalise(spoken);
    if q.is_empty() {
        return None;
    }
    let mut candidate = q.as_str();
    loop {
        if let Some(exact) = entries
            .iter()
            .filter(|e| normalise(&e.label) == candidate)
            .min_by(|a, b| a.label.len().cmp(&b.label.len()).then(a.label.cmp(&b.label)))
        {
            return Some(exact);
        }
        match candidate.split_once(' ') {
            Some((word, tail)) if OPEN_FILLERS.contains(&word) => candidate = tail,
            _ => break,
        }
    }
    if OPEN_FILLERS.contains(&candidate) {
        return None;
    }
    find_normalised(entries, candidate, near).or_else(|| {
        let english = FRENCH_NAMES.iter().find(|(fr, _)| *fr == candidate)?.1;
        find_normalised(entries, english, near)
    })
}

/// How many edits a near spelling may have: one for short words, where two
/// turn "chrome" into "home", two from seven letters.
fn near_enough(q: &str, name: &str) -> Option<usize> {
    let shorter = q.chars().count().min(name.chars().count());
    let d = levenshtein(q, name);
    (d <= if shorter >= 7 { 2 } else { 1 }).then_some(d)
}

fn find_normalised<'a>(entries: &'a [AppEntry], q: &str, near: bool) -> Option<&'a AppEntry> {
    let q = q.to_string();
    let q_words: Vec<&str> = q.split(' ').collect();
    let rank = |e: &AppEntry| -> Option<usize> {
        let l = normalise(&e.label);
        let words: Vec<&str> = l.split(' ').collect();
        if l == q {
            return Some(0);
        }
        // "visual studio" → "Visual Studio Code".
        if words.starts_with(&q_words) {
            return Some(1);
        }
        // "word" → "Microsoft Word", "studio code" → "Visual Studio Code".
        if words.windows(q_words.len()).any(|w| w == q_words.as_slice()) {
            return Some(2);
        }
        // Whisper's spelling: "figmah", "spotfy". Not for short names, which
        // would match anything.
        if near && q.chars().count() >= 4 {
            let d = std::iter::once(l.as_str()).chain(words.iter().copied()).filter_map(|w| near_enough(&q, w)).min();
            if let Some(d) = d {
                return Some(3 + d);
            }
        }
        None
    };
    entries
        .iter()
        .filter_map(|e| rank(e).map(|r| (r, e)))
        .min_by(|(ra, a), (rb, b)| ra.cmp(rb).then(a.label.len().cmp(&b.label.len())).then(a.label.cmp(&b.label)))
        .map(|(_, e)| e)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for j in 1..=b.len() {
            let cur = row[j];
            row[j] = (row[j] + 1).min(row[j - 1] + 1).min(prev + usize::from(ca != b[j - 1]));
            prev = cur;
        }
    }
    row[b.len()]
}

/// Windows' "All apps" list, now.
#[cfg(windows)]
pub fn catalog() -> Vec<AppEntry> {
    // Its own thread: the shell's COM objects need an apartment of their own.
    std::thread::spawn(|| entries(apps_folder::items().into_iter().chain(start_menu::shortcuts()))).join().unwrap_or_default()
}

/// Whether `target` is a shortcut from the Start menu's own folders, opened
/// as a file rather than through AppsFolder (launch.rs).
#[cfg(windows)]
pub fn is_start_shortcut(target: &str) -> bool {
    start_menu::is_shortcut(target)
}

/// The Start menu's shortcuts (`*.lnk` under both Programs folders), by
/// name. AppsFolder normally lists them all, but an app whose registration
/// is broken can be missing there while its shortcut is in Start (seen
/// with Google Chrome). They come after AppsFolder's items, so an app
/// AppsFolder lists keeps its entry there (one entry per name).
#[cfg(windows)]
mod start_menu {
    use std::path::{Path, PathBuf};

    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_CommonPrograms, FOLDERID_Programs, KF_FLAG_DEFAULT};

    /// Folders deep enough for "Vendor\App.lnk" and one more level.
    const DEPTH: usize = 3;
    const MAX: usize = 2000;

    fn roots() -> Vec<PathBuf> {
        [FOLDERID_Programs, FOLDERID_CommonPrograms]
            .iter()
            .filter_map(|id| unsafe {
                let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
                let path = raw.to_string().ok();
                CoTaskMemFree(Some(raw.0 as *const _));
                path.map(PathBuf::from)
            })
            .collect()
    }

    pub fn shortcuts() -> Vec<(String, String)> {
        let mut out = Vec::new();
        for root in roots() {
            walk(&root, DEPTH, &mut out);
        }
        out
    }

    fn walk(dir: &Path, depth: usize, out: &mut Vec<(String, String)>) {
        let Ok(list) = std::fs::read_dir(dir) else { return };
        for item in list.flatten() {
            if out.len() >= MAX {
                return;
            }
            let Ok(kind) = item.file_type() else { continue };
            let path = item.path();
            if kind.is_dir() && depth > 1 {
                walk(&path, depth - 1, out);
            } else if kind.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
                if let (Some(name), Some(target)) = (path.file_stem().and_then(|s| s.to_str()), path.to_str()) {
                    out.push((name.to_string(), target.to_string()));
                }
            }
        }
    }

    pub fn is_shortcut(target: &str) -> bool {
        let path = Path::new(target);
        path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
            && !path.components().any(|c| matches!(c, std::path::Component::ParentDir))
            && roots().iter().any(|root| path.starts_with(root))
            && path.is_file()
    }
}

#[cfg(windows)]
mod apps_folder {
    use windows::core::PWSTR;
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{
        BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, SHGetKnownFolderItem, KF_FLAG_DEFAULT, SIGDN,
        SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
    };

    /// (name, AppID) of every item in AppsFolder, as Get-StartApps lists them.
    pub fn items() -> Vec<(String, String)> {
        let mut out = Vec::new();
        unsafe {
            let inited = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            if let Ok(folder) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) {
                if let Ok(list) = folder.BindToHandler::<_, IEnumShellItems>(None::<&IBindCtx>, &BHID_EnumItems) {
                    loop {
                        let mut item = [None];
                        let mut fetched = 0u32;
                        if list.Next(&mut item, Some(&mut fetched)).is_err() || fetched == 0 {
                            break;
                        }
                        let Some(item) = item[0].take() else { break };
                        if let (Some(name), Some(id)) = (name(&item, SIGDN_NORMALDISPLAY), name(&item, SIGDN_PARENTRELATIVEPARSING)) {
                            out.push((name, id));
                        }
                    }
                }
            }
            if inited {
                CoUninitialize();
            }
        }
        out
    }

    unsafe fn name(item: &IShellItem, kind: SIGDN) -> Option<String> {
        let raw: PWSTR = unsafe { item.GetDisplayName(kind) }.ok()?;
        let text = unsafe { raw.to_string() }.ok();
        unsafe { CoTaskMemFree(Some(raw.0 as *const _)) };
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str) -> AppEntry {
        entry_for(label, &format!("Vendor.{}_1234!App", label.replace(' ', ""))).unwrap()
    }

    fn list() -> Vec<AppEntry> {
        ["Figma", "Figma Agent", "Visual Studio Code", "Microsoft Word", "WordPad", "Spotify", "Notepad", "Notepad++", "Paint"]
            .iter()
            .map(|l| entry(l))
            .collect()
    }

    fn found(spoken: &str) -> Option<String> {
        find(&list(), spoken).map(|e| e.label.clone())
    }

    #[test]
    fn a_spoken_name_finds_the_app_start_shows() {
        assert_eq!(found("Figma").as_deref(), Some("Figma"), "exact beats a longer name");
        assert_eq!(found("figma.").as_deref(), Some("Figma"));
        assert_eq!(found("Visual Studio").as_deref(), Some("Visual Studio Code"));
        assert_eq!(found("studio code").as_deref(), Some("Visual Studio Code"));
        assert_eq!(found("word").as_deref(), Some("Microsoft Word"), "a whole word, not WordPad");
        assert_eq!(found("Spotfy").as_deref(), Some("Spotify"), "Whisper's spelling");
        assert_eq!(found("notepad").as_deref(), Some("Notepad"));
        assert_eq!(found("Paint").as_deref(), Some("Paint"));
    }

    #[test]
    fn a_short_name_is_not_taken_for_another_two_letters_away() {
        let list: Vec<AppEntry> = ["Dev Home", "Spotify", "Google Play Games"].iter().map(|l| entry(l)).collect();
        assert_eq!(find(&list, "chrome").map(|e| e.label.as_str()), None, "not Dev Home");
        assert_eq!(find(&list, "Google Chrome").map(|e| e.label.as_str()), None);
        assert_eq!(find(&list, "spotfy").map(|e| e.label.as_str()), Some("Spotify"), "one edit still matches");
        assert_eq!(near_enough("chrome", "home"), None);
        assert_eq!(near_enough("calculater", "calculator"), Some(1));
        assert_eq!(near_enough("calculatr0", "calculator"), Some(2), "two edits from seven letters");
    }

    #[test]
    fn a_typed_name_takes_no_near_spelling() {
        let list = list();
        assert_eq!(find_typed(&list, "Spotfy"), None);
        assert_eq!(find_typed(&list, "Spotify").map(|e| e.label.as_str()), Some("Spotify"));
        assert_eq!(find_typed(&list, "word").map(|e| e.label.as_str()), Some("Microsoft Word"), "whole words still do");
        assert_eq!(find_typed(&list, "the app Visual Studio").map(|e| e.label.as_str()), Some("Visual Studio Code"));
    }

    #[test]
    fn a_start_menu_shortcut_is_an_app_after_appsfolder_items() {
        let found: Vec<(String, String)> = entries([
            ("Google Chrome".to_string(), r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Google Chrome.lnk".to_string()),
            ("Paint".to_string(), "Microsoft.Paint_8wekyb3d8bbwe!App".to_string()),
            ("paint".to_string(), r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Paint.lnk".to_string()),
        ])
        .into_iter()
        .map(|e| (e.label, e.target))
        .collect();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "Google Chrome");
        assert_eq!(found[1].1, "Microsoft.Paint_8wekyb3d8bbwe!App", "AppsFolder's entry wins");
    }

    #[cfg(windows)]
    #[test]
    fn only_shortcuts_inside_start_menu_folders_open_as_files() {
        assert!(!is_start_shortcut(r"C:\Windows\System32\calc.exe"));
        assert!(!is_start_shortcut(r"C:\Users\Public\Desktop\x.lnk"));
        assert!(!is_start_shortcut(r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\..\..\x.lnk"));
        assert!(!is_start_shortcut("Microsoft.Paint_8wekyb3d8bbwe!App"));
        let chrome = r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Google Chrome.lnk";
        if std::path::Path::new(chrome).is_file() {
            assert!(is_start_shortcut(chrome));
        }
    }

    #[test]
    fn windows_own_apps_answer_to_their_french_names() {
        let list: Vec<AppEntry> = ["Calculator", "Notepad", "Settings", "File Explorer", "Calculatrice Pro"].iter().map(|l| entry(l)).collect();
        let find = |s: &str| super::find(&list, s).map(|e| e.label.as_str());
        assert_eq!(find("calculatrice"), Some("Calculatrice Pro"), "an app really called that comes first");
        let list: Vec<AppEntry> = ["Calculator", "Notepad", "Settings", "File Explorer"].iter().map(|l| entry(l)).collect();
        let find = |s: &str| super::find(&list, s).map(|e| e.label.as_str());
        assert_eq!(find("Calculatrice"), Some("Calculator"));
        assert_eq!(find("bloc-notes"), Some("Notepad"));
        assert_eq!(find("paramètres"), Some("Settings"));
        assert_eq!(find("l'explorateur de fichiers"), Some("File Explorer"));
        assert_eq!(find("explorateur de fichiers"), Some("File Explorer"));
    }

    #[test]
    fn app_queries_accept_common_english_and_french_articles() {
        let list: Vec<AppEntry> = ["Notion", "Notepad", "Settings", "Calculator"].iter().map(|l| entry(l)).collect();
        for prefix in [
            "a ", "an ", "the ", "an app ", "an application ", "the app ",
            "un ", "une ", "des ", "le ", "la ", "les ", "l'", "l’",
            "une app ", "une application ", "une appli ", "l'app ", "l’application ", "l’appli ",
        ] {
            let spoken = format!("{prefix}Notion");
            assert_eq!(find(&list, &spoken).map(|e| e.label.as_str()), Some("Notion"), "{spoken}");
            assert_eq!(find(&list, prefix), None, "an article or app qualifier is not a name: {prefix}");
        }
        for (spoken, want) in [("un Bloc-notes", "Notepad"), ("les paramètres", "Settings"), ("moi la calculatrice", "Calculator")] {
            assert_eq!(find(&list, spoken).map(|e| e.label.as_str()), Some(want), "{spoken}");
        }
    }

    #[test]
    fn exact_article_led_app_names_win_before_stripping_or_fuzzy_matching() {
        for (label, shorter) in [
            ("The Witness", "Witness"), ("A Way Out", "Way Out"), ("An Editor", "Editor"),
            ("Un Monde", "Monde"), ("Une Histoire", "Histoire"), ("Des Chiffres", "Chiffres"),
            ("Le Monde", "Monde"), ("La Suite", "Suite"), ("Les Sims", "Sims"), ("L'Éditeur", "Éditeur"),
            ("App Inventor", "Inventor"), ("À la carte", "Carte"),
        ] {
            let list = vec![entry(shorter), entry(label)];
            for prefix in ["", "an app ", "the application ", "une appli "] {
                let spoken = format!("{prefix}{label}");
                assert_eq!(find(&list, &spoken).map(|e| e.label.as_str()), Some(label), "{spoken}");
            }
        }
        let list = vec![entry("A Motion"), entry("Notion")];
        assert_eq!(find(&list, "a Notion").map(|e| e.label.as_str()), Some("Notion"));
    }

    #[test]
    fn article_fallback_preserves_name_words_and_existing_matching() {
        for label in [
            "Android Studio", "Aegis", "Leslie", "Unity", "Unearth", "Desmos", "Create an App",
            "Design a Home", "Tous les outils", "Dessine un plan", "Crée une application", "Gestionnaire des tâches",
        ] {
            let list = vec![entry(label)];
            assert_eq!(find(&list, &format!("the app {label}")).map(|e| e.label.as_str()), Some(label));
        }
        assert_eq!(found("the app Visual Studio").as_deref(), Some("Visual Studio Code"));
        assert_eq!(found("an app studio code").as_deref(), Some("Visual Studio Code"));
        assert_eq!(found("une application Spotfy").as_deref(), Some("Spotify"));
    }

    #[test]
    fn nothing_close_enough_is_nothing() {
        assert_eq!(found("Photoshop"), None);
        assert_eq!(found(""), None);
        assert_eq!(found("pai"), None, "short names need an exact word");
        assert_eq!(found("mes mails"), None);
    }

    #[test]
    fn ids_are_what_the_policy_accepts_and_tell_entries_apart() {
        let a = id_for("Notepad++", r"{6D809377-6AF0-444B-8957-A3773F02200E}\Notepad++\notepad++.exe");
        let b = id_for("Notepad++", r"C:\Tools\notepad++.exe");
        assert!(a.starts_with("notepad-"), "{a}");
        assert_ne!(a, b);
        for id in [a, b, id_for("Café Bar — Pro", "x"), id_for("日本", "y"), id_for(&"x".repeat(200), "z")] {
            assert!(id.len() <= 64 && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'), "{id}");
        }
        assert_eq!(
            id_for("Calculator", "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"),
            id_for("Calculator", "microsoft.windowscalculator_8wekyb3d8bbwe!app")
        );
    }

    #[test]
    fn uninstallers_documents_and_links_are_not_apps() {
        for name in ["Uninstall Figma", "Désinstaller Foo", "Foo Readme", "Help", "Foo Website", "Release Notes"] {
            assert!(entry_for(name, "Vendor.App!App").is_none(), "{name}");
        }
        for target in ["https://example.com/support", r"C:\Docs\manual.pdf", r"{GUID}\Foo\notes.txt", "x\"; calc", "a\nb", ""] {
            assert!(entry_for("Foo", target).is_none(), "{target}");
        }
        assert_eq!(entry_for(" Figma ", "com.figma.Desktop").unwrap().label, "Figma");
    }

    #[test]
    fn the_list_keeps_one_entry_per_name() {
        let labels: Vec<String> = entries([
            ("Figma".to_string(), "com.figma.Desktop".to_string()),
            ("figma".to_string(), r"C:\Other\Figma.exe".to_string()),
            ("Uninstall Paint".to_string(), "x.exe".to_string()),
            ("Paint".to_string(), "Microsoft.Paint_8wekyb3d8bbwe!App".to_string()),
        ])
        .into_iter()
        .map(|e| e.label)
        .collect();
        assert_eq!(labels, ["Figma", "Paint"]);
    }

    /// The real list on this computer (opt-in: it depends on what is installed).
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn this_computers_start_list() {
        let list = catalog();
        println!("{} apps", list.len());
        for name in ["calculatrice", "Calculator", "Notepad", "Spotify", "Visual Studio Code", "paint", "Discord", "Google Chrome", "chrome"] {
            println!("{name:>20} → {:?}", find(&list, name).map(|e| (&e.label, &e.target)));
        }
        assert!(list.len() > 10);
    }
}
