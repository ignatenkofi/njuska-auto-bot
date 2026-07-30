//! UI-string localization (issue #33).
//!
//! The audience is Russian-speaking expats in Serbia, so Russian is the
//! default; Serbian is offered as an alternative. Every user-facing string the
//! bot *authors* lives here as a method on [`Lang`], keyed by message id — the
//! "small table keyed by message id" the issue asked for. No `fluent`/ICU
//! machinery: at this scale a per-language `match` is plenty, and it keeps the
//! copy greppable and the compiler in charge of completeness.
//!
//! ## What is NOT here
//!
//! * Listing content (titles, cities, prices) comes from the site and is shown
//!   verbatim — see [`crate::telegram::format_listing_html`].
//! * Brand/model *display names* are proper nouns (Audi, Golf, …) and the live
//!   `/filter` catalog pulls them from the site (already Serbian), so they stay
//!   in [`crate::commands::catalog`], not here.
//! * The poll loop's operator alerts in [`crate::bot`] are deliberately English
//!   diagnostics, not end-user UI — left as-is.
//! * The `setMyCommands` autocomplete menu is bot-global (one payload for every
//!   user), so it can't be localized per-user without Telegram `language_code`
//!   scoping; its descriptions stay in the `#[command(description=…)]` derive
//!   (Russian). `/help`, a per-user reply, *is* localized — see
//!   [`crate::commands`].
//!
//! ## Layering
//!
//! `i18n` is foundational: it may depend on [`crate::models`] and
//! [`crate::version`], but nothing in `commands` — the pickers and handlers
//! depend on `i18n`, never the reverse. That keeps the string layer reusable
//! and free of Telegram types.

use std::str::FromStr;

use crate::models::SearchFilter;

/// Placeholder shown for an unset filter field. Same glyph in every language.
const DASH: &str = "—";

/// UI languages the bot can render.
///
/// `Copy` because it's a plain C-like enum (two unit variants) — passing it by
/// value is a register move, so handlers and keyboard builders take `Lang`
/// rather than `&Lang`. `Default` is `Ru`: the primary audience is
/// Russian-speaking, and a fresh deployment with no `BOT_LANG` set should speak
/// Russian.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    Ru,
    Sr,
}

/// Error from parsing a language code (`BOT_LANG`, `/language <x>`, the stored
/// `lang` setting). `thiserror` (not `anyhow`) because callers — the config
/// loader and the `/language` handler — want to *match* on "was it a bad code?"
/// vs. surface it, which is exactly what a typed module error is for.
#[derive(Debug, thiserror::Error)]
#[error("unknown language code {0:?}; expected `ru` or `sr`")]
pub struct ParseLangError(String);

impl FromStr for Lang {
    type Err = ParseLangError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ru" => Ok(Lang::Ru),
            "sr" => Ok(Lang::Sr),
            other => Err(ParseLangError(other.to_string())),
        }
    }
}

impl Lang {
    /// Every supported language, in menu order. Drives the `/language` picker
    /// so a new variant surfaces automatically once its `match` arms exist.
    pub const ALL: &'static [Lang] = &[Lang::Ru, Lang::Sr];

    /// Stable machine code — what we persist in `runtime_settings` and put in
    /// the picker's callback data. Round-trips through [`FromStr`].
    pub fn as_code(self) -> &'static str {
        match self {
            Lang::Ru => "ru",
            Lang::Sr => "sr",
        }
    }

    /// The language's own name, for the picker button and confirmations.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::Ru => "Русский",
            Lang::Sr => "Srpski",
        }
    }

    // -- /start ---------------------------------------------------------------

    pub fn start(self) -> &'static str {
        match self {
            Lang::Ru => {
                "👋 <b>Привет!</b> Я NjuskaAutoBot — слежу за объявлениями на \
                 polovniautomobili.com и кидаю тебе сюда новые.\n\n\
                 /help — список команд\n\
                 /status — текущая конфигурация\n\
                 /pause /resume — поставить на паузу и возобновить"
            }
            Lang::Sr => {
                "👋 <b>Zdravo!</b> Ja sam NjuskaAutoBot — pratim oglase na \
                 polovniautomobili.com i šaljem ti ovde nove.\n\n\
                 /help — spisak komandi\n\
                 /status — trenutna konfiguracija\n\
                 /pause /resume — pauziraj i nastavi"
            }
        }
    }

    // -- /status --------------------------------------------------------------

    /// Full `/status` body. `search_url` must already be HTML-attr-escaped by
    /// the caller (i18n stays free of Telegram escaping helpers); the filter
    /// bullet list is rendered here via [`Lang::filter_summary`].
    pub fn status(
        self,
        f: &SearchFilter,
        paused: bool,
        interval_secs: u64,
        count: u64,
        search_url: &str,
    ) -> String {
        let summary = self.filter_summary(f);
        let version = crate::version::VERSION;
        let icon = if paused { "⏸" } else { "▶️" };
        match self {
            Lang::Ru => {
                let state = if paused {
                    "на паузе"
                } else {
                    "работает"
                };
                format!(
                    "<b>Текущая конфигурация</b>\n\n\
                     {icon} Поллинг: <b>{state}</b>, интервал <b>{interval_secs}</b> сек\n\n\
                     <b>Фильтры поиска</b>\n{summary}\n\
                     🔗 <a href=\"{search_url}\">Открыть этот поиск на сайте</a>\n\n\
                     <b>База</b>: {count} объявлений в seen_listings\n\
                     <b>Версия</b>: <code>{version}</code>",
                )
            }
            Lang::Sr => {
                let state = if paused { "pauzirana" } else { "aktivna" };
                format!(
                    "<b>Trenutna konfiguracija</b>\n\n\
                     {icon} Provera: <b>{state}</b>, interval <b>{interval_secs}</b> sek\n\n\
                     <b>Filteri pretrage</b>\n{summary}\n\
                     🔗 <a href=\"{search_url}\">Otvori ovu pretragu na sajtu</a>\n\n\
                     <b>Baza</b>: {count} oglasa u seen_listings\n\
                     <b>Verzija</b>: <code>{version}</code>",
                )
            }
        }
    }

    /// The filter bullet list, shared by `/status`, the `/filter` menu body and
    /// the "saved"/"reset" confirmations. Chassis/gearbox codes render as their
    /// localized labels (out-of-catalog codes fall through to the raw number,
    /// same contract as before #4).
    pub fn filter_summary(self, f: &SearchFilter) -> String {
        // One shared skeleton, per-language field labels — avoids maintaining
        // the seven-line structure twice.
        let (l_brand, l_models, l_chassis, l_gearbox, l_price, l_year, l_wo, yes, no) = match self {
            Lang::Ru => (
                "Марка",
                "Модели",
                "Кузов",
                "КПП",
                "Цена",
                "Год",
                "Без цены",
                "да",
                "нет",
            ),
            Lang::Sr => (
                "Marka",
                "Modeli",
                "Karoserija",
                "Menjač",
                "Cena",
                "Godište",
                "Bez cene",
                "da",
                "ne",
            ),
        };

        let or_dash = |s: Option<String>| s.unwrap_or_else(|| DASH.to_string());
        let list_or_dash = |v: &[String]| {
            if v.is_empty() {
                DASH.to_string()
            } else {
                v.join(", ")
            }
        };
        let codes_or_dash = |v: &[u32], label: &dyn Fn(u32) -> String| {
            if v.is_empty() {
                DASH.to_string()
            } else {
                v.iter().map(|c| label(*c)).collect::<Vec<_>>().join(", ")
            }
        };

        let mut lines = Vec::with_capacity(7);
        lines.push(format!(
            "• {l_brand}: <code>{}</code>",
            or_dash(f.brand.clone())
        ));
        lines.push(format!(
            "• {l_models}: <code>{}</code>",
            list_or_dash(&f.models)
        ));
        lines.push(format!(
            "• {l_chassis}: <code>{}</code>",
            codes_or_dash(&f.chassis, &|c| self.chassis_label(c))
        ));
        lines.push(format!(
            "• {l_gearbox}: <code>{}</code>",
            codes_or_dash(&f.gearbox, &|c| self.gearbox_label(c))
        ));
        lines.push(format!(
            "• {l_price}: <code>{} – {}</code>",
            or_dash(f.price_from.map(|p| p.to_string())),
            or_dash(f.price_to.map(|p| p.to_string())),
        ));
        lines.push(format!(
            "• {l_year}: <code>{} – {}</code>",
            or_dash(f.year_from.map(|y| y.to_string())),
            or_dash(f.year_to.map(|y| y.to_string())),
        ));
        lines.push(format!(
            "• {l_wo}: <code>{}</code>",
            if f.without_price { yes } else { no }
        ));
        lines.join("\n")
    }

    // -- /pause, /resume ------------------------------------------------------

    pub fn pause_already(self) -> &'static str {
        match self {
            Lang::Ru => "ℹ️ Поллинг уже на паузе.",
            Lang::Sr => "ℹ️ Provera je već pauzirana.",
        }
    }

    pub fn paused_ok(self) -> &'static str {
        match self {
            Lang::Ru => "⏸ Поллинг остановлен.",
            Lang::Sr => "⏸ Provera je zaustavljena.",
        }
    }

    pub fn resume_already(self) -> &'static str {
        match self {
            Lang::Ru => "ℹ️ Поллинг уже работает.",
            Lang::Sr => "ℹ️ Provera već radi.",
        }
    }

    pub fn resumed_ok(self) -> &'static str {
        match self {
            Lang::Ru => "▶️ Поллинг возобновлён.",
            Lang::Sr => "▶️ Provera je nastavljena.",
        }
    }

    /// DB-write failure on a state flip (`/pause`, `/resume`) — the ❌ is part
    /// of the message here (the caller sends it verbatim).
    pub fn db_write_failed(self) -> &'static str {
        match self {
            Lang::Ru => "❌ Не смог записать в БД, состояние не меняю.",
            Lang::Sr => "❌ Nisam mogao da upišem u bazu, stanje ne menjam.",
        }
    }

    // -- /interval ------------------------------------------------------------

    pub fn interval_below_min(self, min: u64) -> String {
        match self {
            Lang::Ru => format!("Минимум <b>{min}</b> секунд — это вежливость к сайту."),
            Lang::Sr => format!("Minimum <b>{min}</b> sekundi — to je ljubaznost prema sajtu."),
        }
    }

    pub fn interval_above_max(self, max: u64) -> String {
        match self {
            Lang::Ru => {
                format!("Максимум <b>{max}</b> секунд (неделя) — больше похоже на опечатку.")
            }
            Lang::Sr => {
                format!("Maksimum <b>{max}</b> sekundi (nedelja) — više liči na grešku u kucanju.")
            }
        }
    }

    /// DB-write failure inside `apply_interval`. No leading ❌: `/interval`
    /// prepends one, and the picker path swallows the error entirely.
    pub fn interval_db_write_failed(self) -> &'static str {
        match self {
            Lang::Ru => "Не смог записать в БД, состояние не меняю.",
            Lang::Sr => "Nisam mogao da upišem u bazu, stanje ne menjam.",
        }
    }

    pub fn interval_set_ok(self, secs: u64) -> String {
        match self {
            Lang::Ru => format!("✅ Интервал поллинга: <b>{secs}</b> сек. Применилось сразу."),
            Lang::Sr => format!("✅ Interval provere: <b>{secs}</b> sek. Primenjeno odmah."),
        }
    }

    // -- /setbrand ------------------------------------------------------------

    pub fn setbrand_no_slug(self) -> &'static str {
        match self {
            Lang::Ru => "❌ Не указан slug. Пример: <code>/setbrand smart</code>",
            Lang::Sr => "❌ Slug nije naveden. Primer: <code>/setbrand smart</code>",
        }
    }

    pub fn setbrand_bad_slug(self, slug: &str) -> String {
        match self {
            Lang::Ru => format!(
                "❌ Slug может содержать только буквы a-z, цифры и дефис.\n\
                 Получил: <code>{slug}</code>"
            ),
            Lang::Sr => format!(
                "❌ Slug može sadržati samo slova a-z, cifre i crticu.\n\
                 Dobijeno: <code>{slug}</code>"
            ),
        }
    }

    pub fn setbrand_ok(self, slug: &str) -> String {
        match self {
            Lang::Ru => format!(
                "✅ Марка установлена: <b>{slug}</b>\n\n\
                 (Если для этой марки нет каталога моделей — пользуйся \
                 <code>SEARCH_MODEL</code> в <code>.env</code>.)"
            ),
            Lang::Sr => format!(
                "✅ Marka je postavljena: <b>{slug}</b>\n\n\
                 (Ako za ovu marku nema kataloga modela — koristi \
                 <code>SEARCH_MODEL</code> u <code>.env</code>.)"
            ),
        }
    }

    // -- /dump ----------------------------------------------------------------

    pub fn dump_n_positive(self) -> &'static str {
        match self {
            Lang::Ru => "❌ N должно быть > 0.",
            Lang::Sr => "❌ N mora biti > 0.",
        }
    }

    pub fn dump_db_read_failed(self) -> &'static str {
        match self {
            Lang::Ru => "❌ Не смог прочитать БД, глянь логи.",
            Lang::Sr => "❌ Nisam mogao da pročitam bazu, pogledaj logove.",
        }
    }

    pub fn dump_empty(self) -> &'static str {
        match self {
            Lang::Ru => "📋 База пустая — пока ничего не было сохранено.",
            Lang::Sr => "📋 Baza je prazna — još ništa nije sačuvano.",
        }
    }

    pub fn dump_header_all(self, shown: usize, requested: u32) -> String {
        match self {
            Lang::Ru => format!("📋 Все <b>{shown}</b> объявлений в БД (запрошено {requested}):"),
            Lang::Sr => format!("📋 Svih <b>{shown}</b> oglasa u bazi (zatraženo {requested}):"),
        }
    }

    pub fn dump_header_recent(self, shown: usize) -> String {
        match self {
            Lang::Ru => format!("📋 Последние <b>{shown}</b> объявлений (новейшие сверху):"),
            Lang::Sr => format!("📋 Poslednjih <b>{shown}</b> oglasa (najnoviji na vrhu):"),
        }
    }

    pub fn dump_tail(self, remaining: usize) -> String {
        match self {
            Lang::Ru => format!("\n… и ещё {remaining} — не влезло в одно сообщение."),
            Lang::Sr => format!("\n… i još {remaining} — nije stalo u jednu poruku."),
        }
    }

    // -- /diag ----------------------------------------------------------------

    pub fn diag_title(self) -> &'static str {
        match self {
            Lang::Ru => "🩺 <b>Диагностика фетча</b>",
            Lang::Sr => "🩺 <b>Dijagnostika fetch-a</b>",
        }
    }

    pub fn diag_proxy(self, configured: bool) -> String {
        let (label, on, off) = match self {
            Lang::Ru => ("Прокси", "настроен (CF Worker)", "нет — прямой fetch"),
            Lang::Sr => ("Proxy", "podešen (CF Worker)", "nema — direktan fetch"),
        };
        format!("{label}: <b>{}</b>", if configured { on } else { off })
    }

    pub fn diag_url(self, url_escaped: &str) -> String {
        // "URL" is the same token in both languages; only kept as a method so
        // every /diag line comes from one place.
        format!("URL: <code>{url_escaped}</code>")
    }

    pub fn diag_http_ok(self, bytes: usize) -> String {
        match self {
            Lang::Ru => format!("HTTP: <b>2xx OK</b>, тело {bytes} байт"),
            Lang::Sr => format!("HTTP: <b>2xx OK</b>, telo {bytes} bajtova"),
        }
    }

    pub fn diag_parsed(self, n: usize) -> String {
        match self {
            Lang::Ru => format!("Распарсено объявлений: <b>{n}</b>"),
            Lang::Sr => format!("Parsirano oglasa: <b>{n}</b>"),
        }
    }

    pub fn diag_zero_hint(self) -> &'static str {
        match self {
            Lang::Ru => {
                "⚠️ 0 объявлений: либо фильтр слишком узкий, либо селекторы \
                 устарели (проверь dumps)."
            }
            Lang::Sr => {
                "⚠️ 0 oglasa: ili je filter preuzak, ili su selektori \
                 zastareli (proveri dumps)."
            }
        }
    }

    pub fn diag_pipeline_ok(self) -> &'static str {
        match self {
            Lang::Ru => "✅ Весь конвейер работает.",
            Lang::Sr => "✅ Ceo lanac radi.",
        }
    }

    pub fn diag_fetch_failed(self, err: &str) -> String {
        match self {
            Lang::Ru => format!("❌ Фетч упал: {err}"),
            Lang::Sr => format!("❌ Fetch je pao: {err}"),
        }
    }

    // -- /cancel --------------------------------------------------------------

    pub fn cancel(self) -> &'static str {
        match self {
            Lang::Ru => {
                "ℹ️ У меня нет режима, который надо отменять.\n\n\
                 Если ты внутри диалога <code>/filter</code> — жми <b>↩️ Назад</b> или \
                 <b>✅ Готово</b>.\n\
                 Если ждёшь подтверждения <code>/clear</code> — просто не отправляй \
                 <code>/clear_confirm</code>, истечёт через 30 секунд."
            }
            Lang::Sr => {
                "ℹ️ Nemam režim koji treba otkazati.\n\n\
                 Ako si unutar dijaloga <code>/filter</code> — pritisni <b>↩️ Nazad</b> ili \
                 <b>✅ Gotovo</b>.\n\
                 Ako čekaš potvrdu <code>/clear</code> — jednostavno ne šalji \
                 <code>/clear_confirm</code>, isteći će za 30 sekundi."
            }
        }
    }

    // -- /clear, /clear_confirm ----------------------------------------------

    pub fn clear_armed(self, count: u64) -> String {
        match self {
            Lang::Ru => format!(
                "⚠️ <b>Опасная операция</b>\n\n\
                 Сейчас в seen_listings <b>{count}</b> объявлений.\n\
                 Команда <code>/clear</code> сотрёт их все — после этого следующий \
                 цикл поллинга снова посчитает текущую выдачу новой и зальёт её в чат.\n\n\
                 Чтобы подтвердить, в течение <b>30 секунд</b> отправь \
                 <code>/clear_confirm</code>.\n\n\
                 Иначе ничего не произойдёт."
            ),
            Lang::Sr => format!(
                "⚠️ <b>Opasna operacija</b>\n\n\
                 Trenutno je u seen_listings <b>{count}</b> oglasa.\n\
                 Komanda <code>/clear</code> će ih sve obrisati — nakon toga će sledeći \
                 ciklus provere ponovo smatrati trenutne rezultate novim i poslati ih u chat.\n\n\
                 Da potvrdiš, u roku od <b>30 sekundi</b> pošalji \
                 <code>/clear_confirm</code>.\n\n\
                 U suprotnom se ništa neće dogoditi."
            ),
        }
    }

    pub fn clear_no_pending(self) -> &'static str {
        match self {
            Lang::Ru => "ℹ️ Нет ожидающего /clear. Сначала отправь /clear.",
            Lang::Sr => "ℹ️ Nema /clear na čekanju. Prvo pošalji /clear.",
        }
    }

    pub fn clear_expired(self, secs: u64) -> String {
        match self {
            Lang::Ru => format!("⏱ Время ожидания истекло ({secs} сек). Начни заново — /clear."),
            Lang::Sr => format!("⏱ Vreme čekanja je isteklo ({secs} sek). Počni ponovo — /clear."),
        }
    }

    pub fn clear_done(self, deleted: u64) -> String {
        match self {
            Lang::Ru => format!("✅ Удалено <b>{deleted}</b> объявлений из seen_listings."),
            Lang::Sr => format!("✅ Obrisano <b>{deleted}</b> oglasa iz seen_listings."),
        }
    }

    pub fn clear_delete_failed(self) -> &'static str {
        match self {
            Lang::Ru => "❌ Удаление сорвалось — глянь логи бота.",
            Lang::Sr => "❌ Brisanje nije uspelo — pogledaj logove bota.",
        }
    }

    // -- /filter menu + pickers ----------------------------------------------

    pub fn filter_menu_body(self, f: &SearchFilter, interval_secs: u64) -> String {
        let summary = self.filter_summary(f);
        match self {
            Lang::Ru => format!(
                "🎛 <b>Фильтры и настройки</b>\n\n\
                 ⏱ Интервал поллинга: <b>{interval_secs}</b> сек\n\n\
                 <b>Фильтры</b>\n{summary}\n\n\
                 Жми кнопку для секции, которую хочешь поменять, или <b>Готово</b> когда всё хорошо.",
            ),
            Lang::Sr => format!(
                "🎛 <b>Filteri i podešavanja</b>\n\n\
                 ⏱ Interval provere: <b>{interval_secs}</b> sek\n\n\
                 <b>Filteri</b>\n{summary}\n\n\
                 Pritisni dugme za sekciju koju želiš da promeniš, ili <b>Gotovo</b> kada je sve u redu.",
            ),
        }
    }

    pub fn brand_picker_title(self) -> &'static str {
        match self {
            Lang::Ru => "Выбери марку:",
            Lang::Sr => "Izaberi marku:",
        }
    }

    pub fn chassis_picker_title(self) -> &'static str {
        match self {
            Lang::Ru => "Выбери типы кузова (можно несколько):",
            Lang::Sr => "Izaberi tipove karoserije (može više):",
        }
    }

    pub fn gearbox_picker_title(self) -> &'static str {
        match self {
            Lang::Ru => "Выбери типы КПП (можно несколько):",
            Lang::Sr => "Izaberi tipove menjača (može više):",
        }
    }

    pub fn price_picker_title(self) -> &'static str {
        match self {
            Lang::Ru => "Выбери диапазон цены:",
            Lang::Sr => "Izaberi raspon cene:",
        }
    }

    pub fn year_picker_title(self) -> &'static str {
        match self {
            Lang::Ru => "Выбери диапазон года выпуска:",
            Lang::Sr => "Izaberi raspon godišta:",
        }
    }

    pub fn interval_picker_title(self) -> &'static str {
        match self {
            Lang::Ru => "Выбери интервал поллинга:",
            Lang::Sr => "Izaberi interval provere:",
        }
    }

    // -- saved filter sets (#10, stage 3) ------------------------------------

    pub fn btn_saved_sets(self) -> &'static str {
        match self {
            Lang::Ru => "💾 Наборы фильтров",
            Lang::Sr => "💾 Sačuvani setovi",
        }
    }

    pub fn btn_new_filter(self) -> &'static str {
        match self {
            Lang::Ru => "➕ Новый набор",
            Lang::Sr => "➕ Novi set",
        }
    }

    pub fn btn_draft_menu(self) -> &'static str {
        match self {
            Lang::Ru => "⚙️ Черновик (секции)",
            Lang::Sr => "⚙️ Radna verzija (sekcije)",
        }
    }

    pub fn btn_filter_enable(self) -> &'static str {
        match self {
            Lang::Ru => "▶️ Включить",
            Lang::Sr => "▶️ Uključi",
        }
    }

    pub fn btn_filter_disable(self) -> &'static str {
        match self {
            Lang::Ru => "⏸ Выключить",
            Lang::Sr => "⏸ Isključi",
        }
    }

    pub fn btn_filter_pull(self) -> &'static str {
        match self {
            Lang::Ru => "📤 В черновик",
            Lang::Sr => "📤 U radnu verziju",
        }
    }

    pub fn btn_filter_push(self) -> &'static str {
        match self {
            Lang::Ru => "📥 Из черновика",
            Lang::Sr => "📥 Iz radne verzije",
        }
    }

    pub fn btn_filter_rename(self) -> &'static str {
        match self {
            Lang::Ru => "✏️ Переименовать",
            Lang::Sr => "✏️ Preimenuj",
        }
    }

    pub fn btn_filter_delete(self) -> &'static str {
        match self {
            Lang::Ru => "🗑 Удалить",
            Lang::Sr => "🗑 Obriši",
        }
    }

    pub fn btn_delete_yes(self) -> &'static str {
        match self {
            Lang::Ru => "✅ Да, удалить",
            Lang::Sr => "✅ Da, obriši",
        }
    }

    /// Selector screen. `count == 0` renders the on-ramp explanation: with an
    /// empty table the poll loop still runs the draft filter (stage 2), so
    /// the copy has to say what "saving a set" changes.
    pub fn selector_body(self, count: usize) -> String {
        match (self, count) {
            (Lang::Ru, 0) => "💾 <b>Наборы фильтров</b>\n\n\
                 Сохранённых наборов пока нет — бот опрашивает черновик \
                 (секции из /filter). Настрой черновик и сохрани его: \
                 <code>/save_filter имя</code>. Как только появится первый \
                 набор, опрашиваются только включённые наборы."
                .to_string(),
            (Lang::Sr, 0) => "💾 <b>Sačuvani setovi</b>\n\n\
                 Još nema sačuvanih setova — bot proverava radnu verziju \
                 (sekcije iz /filter). Podesi radnu verziju i sačuvaj je: \
                 <code>/save_filter ime</code>. Čim se pojavi prvi set, \
                 proveravaju se samo uključeni setovi."
                .to_string(),
            (Lang::Ru, n) => format!(
                "💾 <b>Наборы фильтров</b> ({n})\n\n\
                 Опрашиваются только включённые (✅). Жми на набор — карточка \
                 с действиями; поля редактируются через черновик \
                 (📤 в черновик → секции → 📥 из черновика)."
            ),
            (Lang::Sr, n) => format!(
                "💾 <b>Sačuvani setovi</b> ({n})\n\n\
                 Proveravaju se samo uključeni (✅). Pritisni set za karticu \
                 sa akcijama; polja se menjaju kroz radnu verziju \
                 (📤 u radnu verziju → sekcije → 📥 iz radne verzije)."
            ),
        }
    }

    /// Card of one saved set (name already HTML-escaped by the caller).
    pub fn saved_filter_card_body(
        self,
        name_escaped: &str,
        enabled: bool,
        f: &SearchFilter,
    ) -> String {
        let summary = self.filter_summary(f);
        let status = match (self, enabled) {
            (Lang::Ru, true) => "✅ включён — участвует в опросе",
            (Lang::Ru, false) => "⏸ выключен — пропускается",
            (Lang::Sr, true) => "✅ uključen — učestvuje u proveri",
            (Lang::Sr, false) => "⏸ isključen — preskače se",
        };
        match self {
            Lang::Ru => {
                format!("💾 Набор <b>{name_escaped}</b>\n{status}\n\n<b>Поля</b>\n{summary}")
            }
            Lang::Sr => {
                format!("💾 Set <b>{name_escaped}</b>\n{status}\n\n<b>Polja</b>\n{summary}")
            }
        }
    }

    pub fn save_filter_hint(self) -> &'static str {
        match self {
            Lang::Ru => {
                "Создание набора: настрой черновик в /filter и отправь \
                 <code>/save_filter имя</code> — текущие поля черновика сохранятся \
                 под этим именем."
            }
            Lang::Sr => {
                "Novi set: podesi radnu verziju u /filter i pošalji \
                 <code>/save_filter ime</code> — trenutna polja radne verzije \
                 biće sačuvana pod tim imenom."
            }
        }
    }

    pub fn rename_filter_hint(self, name_escaped: &str) -> String {
        match self {
            Lang::Ru => format!(
                "Переименование <b>{name_escaped}</b>: отправь \
                 <code>/rename_filter новое-имя</code>."
            ),
            Lang::Sr => format!(
                "Preimenovanje <b>{name_escaped}</b>: pošalji \
                 <code>/rename_filter novo-ime</code>."
            ),
        }
    }

    pub fn save_filter_saved(self, name_escaped: &str) -> String {
        match self {
            Lang::Ru => format!(
                "✅ Набор <b>{name_escaped}</b> сохранён из черновика. \
                 Опрашиваются только включённые наборы — смотри /filter."
            ),
            Lang::Sr => format!(
                "✅ Set <b>{name_escaped}</b> je sačuvan iz radne verzije. \
                 Proveravaju se samo uključeni setovi — vidi /filter."
            ),
        }
    }

    pub fn save_filter_bad_name(self) -> &'static str {
        match self {
            Lang::Ru => {
                "❌ Имя набора — от 1 до 40 символов: <code>/save_filter имя</code> \
                 (например, /save_filter bmw)."
            }
            Lang::Sr => {
                "❌ Ime seta — od 1 do 40 znakova: <code>/save_filter ime</code> \
                 (na primer, /save_filter bmw)."
            }
        }
    }

    pub fn wizard_storage_error(self) -> &'static str {
        match self {
            Lang::Ru => "❌ Не получилось записать в базу — глянь логи бота.",
            Lang::Sr => "❌ Upis u bazu nije uspeo — pogledaj logove bota.",
        }
    }

    pub fn save_filter_name_taken(self, name_escaped: &str) -> String {
        match self {
            Lang::Ru => format!(
                "❌ Имя <b>{name_escaped}</b> уже занято — выбери другое или \
                 переименуй старый набор."
            ),
            Lang::Sr => format!(
                "❌ Ime <b>{name_escaped}</b> je već zauzeto — izaberi drugo ili \
                 preimenuj stari set."
            ),
        }
    }

    pub fn rename_filter_done(self, name_escaped: &str) -> String {
        match self {
            Lang::Ru => format!("✅ Набор переименован в <b>{name_escaped}</b>."),
            Lang::Sr => format!("✅ Set je preimenovan u <b>{name_escaped}</b>."),
        }
    }

    pub fn rename_filter_no_selection(self) -> &'static str {
        match self {
            Lang::Ru => {
                "❌ Сначала открой набор в /filter (💾 Наборы фильтров) — \
                 переименование применяется к открытому набору."
            }
            Lang::Sr => {
                "❌ Prvo otvori set u /filter (💾 Sačuvani setovi) — \
                 preimenovanje se odnosi na otvoreni set."
            }
        }
    }

    pub fn filter_delete_confirm_body(self, name_escaped: &str) -> String {
        match self {
            Lang::Ru => format!(
                "🗑 Удалить набор <b>{name_escaped}</b>? Его история дедупа \
                 уйдёт вместе с ним."
            ),
            Lang::Sr => format!(
                "🗑 Obrisati set <b>{name_escaped}</b>? Njegova istorija \
                 dedupa ide zajedno sa njim."
            ),
        }
    }

    pub fn filter_deleted(self, name_escaped: &str) -> String {
        match self {
            Lang::Ru => format!("✅ Набор <b>{name_escaped}</b> удалён."),
            Lang::Sr => format!("✅ Set <b>{name_escaped}</b> je obrisan."),
        }
    }

    pub fn filter_pull_done(self, name_escaped: &str) -> String {
        match self {
            Lang::Ru => format!(
                "📤 Поля набора <b>{name_escaped}</b> скопированы в черновик — \
                 редактируй секции и верни их 📥 Из черновика."
            ),
            Lang::Sr => format!(
                "📤 Polja seta <b>{name_escaped}</b> su kopirana u radnu verziju — \
                 izmeni sekcije i vrati ih 📥 Iz radne verzije."
            ),
        }
    }

    pub fn filter_push_done(self, name_escaped: &str) -> String {
        match self {
            Lang::Ru => format!("📥 Черновик записан в набор <b>{name_escaped}</b>."),
            Lang::Sr => format!("📥 Radna verzija je upisana u set <b>{name_escaped}</b>."),
        }
    }

    pub fn filter_gone(self) -> &'static str {
        match self {
            Lang::Ru => "Этого набора уже нет — список обновлён.",
            Lang::Sr => "Tog seta više nema — spisak je osvežen.",
        }
    }

    /// Model-picker title (brand already HTML-escaped by the caller).
    pub fn models_picker_title(self, brand_escaped: &str) -> String {
        match self {
            Lang::Ru => format!("Модели для <b>{brand_escaped}</b> (можно несколько):"),
            Lang::Sr => format!("Modeli za <b>{brand_escaped}</b> (može više):"),
        }
    }

    pub fn models_pick_brand_first(self) -> &'static str {
        match self {
            Lang::Ru => "❌ Сначала выбери марку — без неё список моделей не известен:",
            Lang::Sr => "❌ Prvo izaberi marku — bez nje spisak modela nije poznat:",
        }
    }

    /// Shown when the chosen brand has no model catalog (brand already escaped).
    /// The button name referenced in the text is the localized "Марка"/"Marka".
    pub fn no_models_for_brand(self, brand_escaped: &str) -> String {
        match self {
            Lang::Ru => format!(
                "🤷 Для марки <code>{brand_escaped}</code> у меня нет каталога моделей.\n\n\
                 Поставь модели через <code>SEARCH_MODEL</code> в \
                 <code>.env</code>, либо смени марку через ✏️ Марка."
            ),
            Lang::Sr => format!(
                "🤷 Za marku <code>{brand_escaped}</code> nemam katalog modela.\n\n\
                 Postavi modele preko <code>SEARCH_MODEL</code> u \
                 <code>.env</code>, ili promeni marku preko ✏️ Marka."
            ),
        }
    }

    pub fn brand_custom_hint(self) -> &'static str {
        match self {
            Lang::Ru => {
                "💬 <b>Бренд вне каталога?</b>\n\n\
                 Обычно это не нужно — список марок теперь подтягивается с сайта \
                 целиком. Но если фетч не удался или марки всё равно нет, \
                 отправь <code>/setbrand &lt;slug&gt;</code>.\n\
                 Slug — значение из URL polovniautomobili.com в параметре \
                 <code>brand=</code>.\n\n\
                 Примеры: <code>/setbrand smart</code>, <code>/setbrand suzuki</code>, \
                 <code>/setbrand alfa-romeo</code>.\n\n\
                 Если потом захочешь обратно в каталог — открой ✏️ Марка."
            }
            Lang::Sr => {
                "💬 <b>Marka van kataloga?</b>\n\n\
                 Obično to nije potrebno — spisak marki se sada u celosti \
                 povlači sa sajta. Ali ako fetch ne uspe ili marke i dalje nema, \
                 pošalji <code>/setbrand &lt;slug&gt;</code>.\n\
                 Slug je vrednost iz URL-a polovniautomobili.com u parametru \
                 <code>brand=</code>.\n\n\
                 Primeri: <code>/setbrand smart</code>, <code>/setbrand suzuki</code>, \
                 <code>/setbrand alfa-romeo</code>.\n\n\
                 Ako kasnije poželiš nazad u katalog — otvori ✏️ Marka."
            }
        }
    }

    pub fn filter_saved(self, summary: &str) -> String {
        match self {
            Lang::Ru => format!("✅ Фильтры сохранены.\n\n{summary}"),
            Lang::Sr => format!("✅ Filteri su sačuvani.\n\n{summary}"),
        }
    }

    pub fn filter_todo(self) -> &'static str {
        match self {
            Lang::Ru => {
                "🔧 Эта секция фильтра будет в следующих сессиях.\n\nПока возвращаемся в меню."
            }
            Lang::Sr => {
                "🔧 Ova sekcija filtera stiže u narednim verzijama.\n\nZa sada se vraćamo u meni."
            }
        }
    }

    pub fn reset_confirm(self, summary: &str) -> String {
        match self {
            Lang::Ru => format!(
                "⚠️ <b>Сбросить все фильтры?</b>\n\n\
                 Сейчас стоит:\n{summary}\n\n\
                 После сброса бот будет видеть весь каталог."
            ),
            Lang::Sr => format!(
                "⚠️ <b>Resetovati sve filtere?</b>\n\n\
                 Trenutno je podešeno:\n{summary}\n\n\
                 Nakon resetovanja bot će videti ceo katalog."
            ),
        }
    }

    pub fn filters_reset(self, menu_body: &str) -> String {
        match self {
            Lang::Ru => format!("🧹 <b>Фильтры сброшены.</b>\n\n{menu_body}"),
            Lang::Sr => format!("🧹 <b>Filteri su resetovani.</b>\n\n{menu_body}"),
        }
    }

    // -- /help usage hints ----------------------------------------------------

    /// Hint for a known command called with bad/missing args. `description` is
    /// the command's own derive description (bot-global, so Russian regardless
    /// of `self`); only the surrounding sentence is localized.
    pub fn usage_hint_known(self, name: &str, description: &str) -> String {
        match self {
            Lang::Ru => format!("❌ Не понял аргументы для <code>/{name}</code>.\n{description}"),
            Lang::Sr => {
                format!("❌ Nisam razumeo argumente za <code>/{name}</code>.\n{description}")
            }
        }
    }

    pub fn usage_hint_unknown(self, cmd_escaped: &str) -> String {
        match self {
            Lang::Ru => {
                format!("🤷 Не знаю команду <code>{cmd_escaped}</code>. Список команд — /help.")
            }
            Lang::Sr => {
                format!("🤷 Ne znam komandu <code>{cmd_escaped}</code>. Spisak komandi — /help.")
            }
        }
    }

    // -- /language ------------------------------------------------------------

    /// The language picker screen (prompt + current language). Shown by
    /// `/language` with no argument and after a switch.
    pub fn language_screen(self) -> String {
        let native = self.native_name();
        match self {
            Lang::Ru => format!(
                "🌐 <b>Язык интерфейса</b>\n\n\
                 Текущий: <b>{native}</b>\n\
                 Выбери язык ниже:"
            ),
            Lang::Sr => format!(
                "🌐 <b>Jezik interfejsa</b>\n\n\
                 Trenutni: <b>{native}</b>\n\
                 Izaberi jezik ispod:"
            ),
        }
    }

    /// Confirmation after `/language <code>` switched the language. `self` is
    /// the *new* language, so the whole message reads in the language just set.
    pub fn language_changed(self) -> String {
        let native = self.native_name();
        match self {
            Lang::Ru => format!("✅ Язык переключён на <b>{native}</b>."),
            Lang::Sr => format!("✅ Jezik je prebačen na <b>{native}</b>."),
        }
    }

    pub fn language_bad_arg(self) -> &'static str {
        match self {
            Lang::Ru => "❌ Не знаю такой язык. Доступно: <code>ru</code>, <code>sr</code>.",
            Lang::Sr => "❌ Ne znam taj jezik. Dostupno: <code>ru</code>, <code>sr</code>.",
        }
    }

    // -- Inline-keyboard button labels ---------------------------------------

    pub fn btn_brand(self) -> &'static str {
        match self {
            Lang::Ru => "✏️ Марка",
            Lang::Sr => "✏️ Marka",
        }
    }

    pub fn btn_models(self) -> &'static str {
        match self {
            Lang::Ru => "✏️ Модели",
            Lang::Sr => "✏️ Modeli",
        }
    }

    pub fn btn_chassis(self) -> &'static str {
        match self {
            Lang::Ru => "✏️ Кузов",
            Lang::Sr => "✏️ Karoserija",
        }
    }

    pub fn btn_gearbox(self) -> &'static str {
        match self {
            Lang::Ru => "✏️ КПП",
            Lang::Sr => "✏️ Menjač",
        }
    }

    pub fn btn_price(self) -> &'static str {
        match self {
            Lang::Ru => "✏️ Цена",
            Lang::Sr => "✏️ Cena",
        }
    }

    pub fn btn_year(self) -> &'static str {
        match self {
            Lang::Ru => "✏️ Год",
            Lang::Sr => "✏️ Godište",
        }
    }

    pub fn btn_interval(self) -> &'static str {
        match self {
            Lang::Ru => "⏱ Интервал",
            Lang::Sr => "⏱ Interval",
        }
    }

    pub fn btn_reset(self) -> &'static str {
        match self {
            Lang::Ru => "🧹 Сбросить",
            Lang::Sr => "🧹 Resetuj",
        }
    }

    pub fn btn_done(self) -> &'static str {
        match self {
            Lang::Ru => "✅ Готово",
            Lang::Sr => "✅ Gotovo",
        }
    }

    pub fn btn_back(self) -> &'static str {
        match self {
            Lang::Ru => "↩️ Назад",
            Lang::Sr => "↩️ Nazad",
        }
    }

    pub fn btn_reset_yes(self) -> &'static str {
        match self {
            Lang::Ru => "✅ Да, стереть",
            Lang::Sr => "✅ Da, obriši",
        }
    }

    pub fn btn_cancel(self) -> &'static str {
        match self {
            Lang::Ru => "↩️ Отмена",
            Lang::Sr => "↩️ Otkaži",
        }
    }

    pub fn btn_no_filter(self) -> &'static str {
        match self {
            Lang::Ru => "Без фильтра",
            Lang::Sr => "Bez filtera",
        }
    }

    pub fn btn_save(self) -> &'static str {
        match self {
            Lang::Ru => "💾 Сохранить",
            Lang::Sr => "💾 Sačuvaj",
        }
    }

    pub fn btn_back_no_save(self) -> &'static str {
        match self {
            Lang::Ru => "↩️ Назад (без сохранения)",
            Lang::Sr => "↩️ Nazad (bez čuvanja)",
        }
    }

    pub fn btn_back_to_brands_no_save(self) -> &'static str {
        match self {
            Lang::Ru => "↩️ К маркам (без сохранения)",
            Lang::Sr => "↩️ Na marke (bez čuvanja)",
        }
    }

    pub fn btn_to_menu(self) -> &'static str {
        match self {
            Lang::Ru => "🏠 В меню",
            Lang::Sr => "🏠 U meni",
        }
    }

    pub fn btn_brand_skip(self) -> &'static str {
        match self {
            Lang::Ru => "⏭ Без фильтра",
            Lang::Sr => "⏭ Bez filtera",
        }
    }

    pub fn btn_brand_manual(self) -> &'static str {
        match self {
            Lang::Ru => "💬 Ввести вручную",
            Lang::Sr => "💬 Unesi ručno",
        }
    }

    // -- Catalog labels (localized display for stable codes/bounds) -----------

    /// Body-type label for a `chassis[]` code. Out-of-catalog codes (set via
    /// `SEARCH_CHASSIS` in `.env`) render as the raw number so nothing is
    /// silently hidden (#4).
    pub fn chassis_label(self, code: u32) -> String {
        let label = match (self, code) {
            (Lang::Ru, 2627) => "Универсал",
            (Lang::Ru, 2628) => "Купе",
            (Lang::Ru, 2629) => "Хэтчбек",
            (Lang::Ru, 2631) => "Седан",
            (Lang::Ru, 2632) => "Внедорожник",
            (Lang::Ru, 2634) => "Кабриолет",
            (Lang::Sr, 2627) => "Karavan",
            (Lang::Sr, 2628) => "Kupe",
            (Lang::Sr, 2629) => "Hečbek",
            (Lang::Sr, 2631) => "Limuzina",
            (Lang::Sr, 2632) => "Džip/SUV",
            (Lang::Sr, 2634) => "Kabriolet",
            _ => return code.to_string(),
        };
        label.to_string()
    }

    /// Gearbox label for a `gearbox[]` code. Same raw-number fallback as
    /// [`Lang::chassis_label`].
    pub fn gearbox_label(self, code: u32) -> String {
        let label = match (self, code) {
            (Lang::Ru, 3210) => "Механика (4 ст.)",
            (Lang::Ru, 3211) => "Механика (5 ст.)",
            (Lang::Ru, 3212) => "Механика (6 ст.)",
            (Lang::Ru, 10795) => "Автомат / полуавтомат",
            (Lang::Sr, 3210) => "Manuelni (4 brzine)",
            (Lang::Sr, 3211) => "Manuelni (5 brzina)",
            (Lang::Sr, 3212) => "Manuelni (6 brzina)",
            (Lang::Sr, 10795) => "Automatik / poluautomatik",
            _ => return code.to_string(),
        };
        label.to_string()
    }

    /// Poll-interval preset label. Only the seven presets are ever shown, but a
    /// generic fallback keeps the function total.
    pub fn interval_label(self, secs: u64) -> String {
        let label = match (self, secs) {
            (Lang::Ru, 60) => "1 мин",
            (Lang::Ru, 300) => "5 мин",
            (Lang::Ru, 600) => "10 мин",
            (Lang::Ru, 1800) => "30 мин",
            (Lang::Ru, 3600) => "1 час",
            (Lang::Ru, 7200) => "2 часа",
            (Lang::Ru, 86_400) => "сутки",
            (Lang::Sr, 60) => "1 min",
            (Lang::Sr, 300) => "5 min",
            (Lang::Sr, 600) => "10 min",
            (Lang::Sr, 1800) => "30 min",
            (Lang::Sr, 3600) => "1 sat",
            (Lang::Sr, 7200) => "2 sata",
            (Lang::Sr, 86_400) => "1 dan",
            (Lang::Ru, s) => return format!("{s} с"),
            (Lang::Sr, s) => return format!("{s} s"),
        };
        label.to_string()
    }

    /// Price-range preset label. `to == 0` means "no upper bound", `from == 0`
    /// means "no lower bound" — the wire convention shared with the catalog.
    pub fn price_range_label(self, from: u32, to: u32) -> String {
        let label = match (self, from, to) {
            (Lang::Ru, 0, 5_000) => "До 5 000 €",
            (Lang::Ru, 50_000, 0) => "Более 50 000 €",
            (Lang::Sr, 0, 5_000) => "Do 5 000 €",
            (Lang::Sr, 50_000, 0) => "Preko 50 000 €",
            // Bounded interior buckets are numeric-only — identical in both
            // languages, so share one arm.
            (_, 5_000, 10_000) => "5–10 000 €",
            (_, 10_000, 15_000) => "10–15 000 €",
            (_, 15_000, 25_000) => "15–25 000 €",
            (_, 25_000, 50_000) => "25–50 000 €",
            _ => return format!("{from}–{to} €"),
        };
        label.to_string()
    }

    /// Year-range preset label. Same `0`-means-unbounded convention as price.
    pub fn year_range_label(self, from: u32, to: u32) -> String {
        let label = match (self, from, to) {
            (Lang::Ru, 2024, 0) => "2024 и новее",
            (Lang::Ru, 0, 2004) => "До 2005",
            (Lang::Sr, 2024, 0) => "2024 i novije",
            (Lang::Sr, 0, 2004) => "Do 2005",
            (_, 2020, 2023) => "2020–2023",
            (_, 2015, 2019) => "2015–2019",
            (_, 2010, 2014) => "2010–2014",
            (_, 2005, 2009) => "2005–2009",
            _ => return format!("{from}–{to}"),
        };
        label.to_string()
    }
}

/// Serbian `/help` body. Russian `/help` reuses teloxide's `descriptions()`
/// derive (see [`crate::commands`]), so only the Serbian block is hand-written
/// here — kept parallel to the `#[command(description=…)]` attributes. A drift
/// test in `commands::mod` pins that every command name appears below.
pub fn help_sr() -> &'static str {
    "Komande NjuskaAutoBot-a:\n\n\
     /start — Pozdrav i kratka pomoć.\n\
     /help — Spisak komandi.\n\
     /status — Trenutna konfiguracija i stanje.\n\
     /pause — Pauziraj proveru.\n\
     /resume — Nastavi proveru.\n\
     /interval — Interval provere u sekundama (60..604800). Primer: /interval 300\n\
     /clear — Pripremi brisanje istorije.\n\
     /clear_confirm — Potvrdi brisanje (30 sek nakon /clear).\n\
     /filter — Podesi filtere pretrage kroz dijalog.\n\
     /save_filter — Sačuvaj radnu verziju filtera kao imenovani set. Primer: /save_filter bmw\n\
     /rename_filter — Preimenuj otvoreni set (otvori ga kroz /filter). Primer: /rename_filter kabrio\n\
     /setbrand — Rezervni ručni unos marke (slug) — ako se katalog sa sajta ne \
     povuče. Obično je lakše izabrati u ✏️ Marka. Primer: /setbrand smart\n\
     /dump — Prikaži poslednjih N sačuvanih oglasa (1-25). Primer: /dump 10\n\
     /cancel — Napomena: komande bez režima ne treba otkazivati.\n\
     /diag — Jednokratna provera fetch-a: mreža, proxy, parser.\n\
     /version — Verzija bota (crate + git SHA).\n\
     /language — Promeni jezik interfejsa (ru / sr)."
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::*;

    #[test]
    fn lang_code_round_trips_through_from_str() {
        for l in Lang::ALL {
            assert_eq!(l.as_code().parse::<Lang>().unwrap(), *l);
        }
        // Case-insensitive and whitespace-tolerant.
        assert_eq!("RU".parse::<Lang>().unwrap(), Lang::Ru);
        assert_eq!("  sr ".parse::<Lang>().unwrap(), Lang::Sr);
        // Unknown codes error, carrying the offending value.
        let err = "de".parse::<Lang>().unwrap_err().to_string();
        assert!(err.contains("de"), "{err}");
    }

    #[test]
    fn default_language_is_russian() {
        // The audience is Russian-speaking; a fresh deploy must default to Ru.
        assert_eq!(Lang::default(), Lang::Ru);
    }

    #[test]
    fn chassis_label_maps_known_codes_and_passes_through_unknown() {
        // Moved here from the catalog when display text became language-aware.
        assert_eq!(Lang::Ru.chassis_label(2634), "Кабриолет");
        assert_eq!(Lang::Ru.chassis_label(2632), "Внедорожник");
        assert_eq!(Lang::Sr.chassis_label(2634), "Kabriolet");
        // Not in the catalog (e.g. set via SEARCH_CHASSIS in .env) — raw code,
        // in either language.
        assert_eq!(Lang::Ru.chassis_label(9999), "9999");
        assert_eq!(Lang::Sr.chassis_label(9999), "9999");
    }

    #[test]
    fn gearbox_label_maps_known_codes_and_passes_through_unknown() {
        assert_eq!(Lang::Ru.gearbox_label(3211), "Механика (5 ст.)");
        assert_eq!(Lang::Ru.gearbox_label(10795), "Автомат / полуавтомат");
        assert_eq!(Lang::Sr.gearbox_label(10795), "Automatik / poluautomatik");
        assert_eq!(Lang::Ru.gearbox_label(9999), "9999");
    }

    #[test]
    fn filter_summary_renders_chassis_as_labels() {
        let f = SearchFilter {
            chassis: vec![2634, 9999],
            ..Default::default()
        };
        assert!(Lang::Ru.filter_summary(&f).contains("Кабриолет, 9999"));
        assert!(Lang::Sr.filter_summary(&f).contains("Kabriolet, 9999"));
    }

    #[test]
    fn filter_summary_renders_gearbox_as_labels() {
        // Known codes render as their labels; out-of-catalog codes pass through
        // raw — same contract as chassis, in both languages.
        let f = SearchFilter {
            gearbox: vec![10795, 8888],
            ..Default::default()
        };
        assert!(
            Lang::Ru
                .filter_summary(&f)
                .contains("КПП: <code>Автомат / полуавтомат, 8888</code>")
        );
        assert!(
            Lang::Sr
                .filter_summary(&f)
                .contains("Menjač: <code>Automatik / poluautomatik, 8888</code>")
        );
    }

    #[test]
    fn filter_summary_renders_dashes_for_empty_filter() {
        let ru = Lang::Ru.filter_summary(&SearchFilter::default());
        // Every unset field shows an em-dash placeholder, not an empty gap.
        assert!(ru.contains("Марка: <code>—</code>"), "{ru}");
        assert!(ru.contains("Модели: <code>—</code>"), "{ru}");
        assert!(ru.contains("КПП: <code>—</code>"), "{ru}");
        assert!(ru.contains("Цена: <code>— – —</code>"), "{ru}");

        let sr = Lang::Sr.filter_summary(&SearchFilter::default());
        assert!(sr.contains("Marka: <code>—</code>"), "{sr}");
        assert!(sr.contains("Cena: <code>— – —</code>"), "{sr}");
    }

    #[test]
    fn range_labels_match_the_shipped_presets() {
        // The keyboard round-trip test pins "✓ 5–10 000 €"; make sure the label
        // that feeds it is exactly that, and identical across languages for the
        // numeric-only interior buckets.
        assert_eq!(Lang::Ru.price_range_label(5_000, 10_000), "5–10 000 €");
        assert_eq!(Lang::Sr.price_range_label(5_000, 10_000), "5–10 000 €");
        assert_eq!(Lang::Ru.price_range_label(0, 5_000), "До 5 000 €");
        assert_eq!(Lang::Sr.price_range_label(0, 5_000), "Do 5 000 €");
        assert_eq!(Lang::Ru.year_range_label(0, 2004), "До 2005");
        assert_eq!(Lang::Sr.year_range_label(2024, 0), "2024 i novije");
    }

    #[test]
    fn every_message_is_non_empty_in_every_language() {
        // Cheap guard against an accidentally-blank arm slipping in.
        let f = SearchFilter::default();
        for l in Lang::ALL {
            let l = *l;
            assert!(!l.start().is_empty());
            assert!(!l.filter_summary(&f).is_empty());
            assert!(!l.status(&f, false, 600, 0, "http://x").is_empty());
            assert!(!l.filter_menu_body(&f, 600).is_empty());
            assert!(!l.brand_custom_hint().is_empty());
            assert!(!l.cancel().is_empty());
            assert!(!l.clear_armed(3).is_empty());
            assert!(!l.language_screen().is_empty());
            assert!(!l.btn_done().is_empty());
            assert!(!l.native_name().is_empty());
        }
    }
}
