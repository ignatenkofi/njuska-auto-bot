//! Inline-keyboard builders and the callback-data constants they emit.
//!
//! Every `InlineKeyboardButton::callback` carries a `data: String`. Telegram
//! caps this at 64 bytes. We namespace everything with `f:` for "filter",
//! and parse in `handle_callback` (in `mod.rs`) by splitting on `:`. Keeping
//! data short and structured means we never need a per-user state machine —
//! the button itself encodes "what the user wants to happen next".

use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

use super::catalog::{BRANDS, CHASSIS, INTERVAL_PRESETS};

/// Show the top-level filter menu.
pub(super) const CB_FILTER_MENU: &str = "f:menu";
/// Show the brand picker.
pub(super) const CB_FILTER_BRAND_PICKER: &str = "f:brand_picker";
/// Clear the brand filter (set to None).
pub(super) const CB_FILTER_BRAND_CLEAR: &str = "f:brand_clear";
/// Prefix for "set the brand to this slug" buttons: `f:brand_set:bmw`.
pub(super) const CB_FILTER_BRAND_SET_PREFIX: &str = "f:brand_set:";
/// Show a hint asking the user to type `/setbrand <slug>` for brands not in
/// the catalog. Pure UI guidance — no state change.
pub(super) const CB_FILTER_BRAND_CUSTOM_HINT: &str = "f:brand_custom_hint";
/// Close the filter menu — last edit leaves a "saved" state, no keyboard.
pub(super) const CB_FILTER_DONE: &str = "f:done";
/// Placeholder for sections not yet implemented (models, price, year).
/// Currently unused — all sections are real after sessions 3.1-3.4. Kept as
/// the scaffold for any future filter section that lands in stages.
#[allow(dead_code)]
pub(super) const CB_FILTER_TODO: &str = "f:todo";

// Interval picker — single-tap commit from a preset list. The picker only
// offers values ≥ MIN_POLL_INTERVAL_SECS, so we don't need to re-validate
// at the callback layer. Custom values still go through `/interval N`.
pub(super) const CB_FILTER_INTERVAL_PICKER: &str = "f:interval_picker";
pub(super) const CB_FILTER_INTERVAL_SET_PREFIX: &str = "f:interval_set:";

// "Reset all filters" two-step. Same pattern as `/clear` / `/clear_confirm`
// but inline-keyboard driven: tap [🧹 Сбросить] → confirmation prompt with
// [✅ Да] [↩️ Отмена]; only [✅ Да] (= `CB_FILTER_RESET_APPLY`) actually wipes.
pub(super) const CB_FILTER_RESET_CONFIRM: &str = "f:reset_confirm";
pub(super) const CB_FILTER_RESET_APPLY: &str = "f:reset_apply";

// Chassis multi-select picker:
//   open       → init draft from runtime, show picker
//   toggle:N   → flip N in the draft, redraw picker
//   save       → write draft to DB+runtime, return to menu
//   (CB_FILTER_MENU = Back, also clears the draft)
pub(super) const CB_FILTER_CHASSIS_PICKER: &str = "f:chassis_picker";
pub(super) const CB_FILTER_CHASSIS_TOGGLE_PREFIX: &str = "f:chassis_toggle:";
pub(super) const CB_FILTER_CHASSIS_SAVE: &str = "f:chassis_save";

// Price + year range pickers. Single-tap commits — no draft state because each
// button carries the *complete* new range, unlike chassis where the user
// builds up a multi-select set.
//
// Callback format: `f:range_set:<field>:<from>:<to>` where `field` is "price"
// or "year" and `from`/`to` are decimal `u32` (0 = "no bound on this side").
// We use one shared prefix and one shared handler — both ranges behave
// identically apart from the field name and the bound type (u32 vs u16).
pub(super) const CB_FILTER_PRICE_PICKER: &str = "f:price_picker";
pub(super) const CB_FILTER_YEAR_PICKER: &str = "f:year_picker";
pub(super) const CB_FILTER_RANGE_SET_PREFIX: &str = "f:range_set:";

// Models multi-select picker. Same shape as chassis (toggle + save + back),
// but the option set depends on the currently-selected brand.
pub(super) const CB_FILTER_MODELS_PICKER: &str = "f:models_picker";
pub(super) const CB_FILTER_MODELS_TOGGLE_PREFIX: &str = "f:models_toggle:";
pub(super) const CB_FILTER_MODELS_SAVE: &str = "f:models_save";

/// Top-level menu keyboard: one button per filter section, plus Done.
/// Each button label is just the section name — values are in the message
/// body. (Putting values on labels would make them long and ugly.)
pub(super) fn filter_menu_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("✏️ Марка", CB_FILTER_BRAND_PICKER),
            InlineKeyboardButton::callback("✏️ Модели", CB_FILTER_MODELS_PICKER),
        ],
        vec![
            InlineKeyboardButton::callback("✏️ Кузов", CB_FILTER_CHASSIS_PICKER),
            InlineKeyboardButton::callback("✏️ Цена", CB_FILTER_PRICE_PICKER),
        ],
        vec![
            InlineKeyboardButton::callback("✏️ Год", CB_FILTER_YEAR_PICKER),
            InlineKeyboardButton::callback("⏱ Интервал", CB_FILTER_INTERVAL_PICKER),
        ],
        vec![
            InlineKeyboardButton::callback("🧹 Сбросить", CB_FILTER_RESET_CONFIRM),
            InlineKeyboardButton::callback("✅ Готово", CB_FILTER_DONE),
        ],
    ])
}

/// Interval picker keyboard. Same single-tap-commit pattern as price/year
/// (no draft state); the `✓` highlights the currently-selected interval if
/// it matches a preset.
pub(super) fn interval_picker_keyboard(current_secs: u64) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = INTERVAL_PRESETS
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(secs, display)| {
                    let prefix = if *secs == current_secs { "✓ " } else { "" };
                    InlineKeyboardButton::callback(
                        format!("{prefix}{display}"),
                        format!("{CB_FILTER_INTERVAL_SET_PREFIX}{secs}"),
                    )
                })
                .collect()
        })
        .collect();
    rows.push(vec![InlineKeyboardButton::callback(
        "↩️ Назад",
        CB_FILTER_MENU,
    )]);
    InlineKeyboardMarkup::new(rows)
}

/// Two-button confirmation keyboard used for the "Reset all filters?" prompt.
/// `[↩️ Отмена]` reuses `CB_FILTER_MENU` so the central back-to-menu logic
/// (drafts cleanup, menu redraw) handles it without a new branch.
pub(super) fn reset_confirm_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("✅ Да, стереть", CB_FILTER_RESET_APPLY),
        InlineKeyboardButton::callback("↩️ Отмена", CB_FILTER_MENU),
    ]])
}

/// Renders a range picker for either price or year.
///
/// Each button represents a complete `(from, to)` range — single-tap commits.
/// The button matching the current filter (if any preset matches) gets a `✓`
/// prefix; the catch-all "Без фильтра" gets `✓` if the current range is
/// `(None, None)`. Custom ranges set via env get no highlight — that's fine,
/// they're an escape hatch, not a UI state.
pub(super) fn range_picker_keyboard(
    field: &str,
    catalog: &[(u32, u32, &str)],
    current: (Option<u32>, Option<u32>),
) -> InlineKeyboardMarkup {
    let mark = |btn_from: u32, btn_to: u32| -> &'static str {
        let cf = if btn_from == 0 { None } else { Some(btn_from) };
        let ct = if btn_to == 0 { None } else { Some(btn_to) };
        if (cf, ct) == current { "✓ " } else { "" }
    };

    // 2 buttons per row for the presets.
    let mut rows: Vec<Vec<InlineKeyboardButton>> = catalog
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(from, to, display)| {
                    InlineKeyboardButton::callback(
                        format!("{}{}", mark(*from, *to), display),
                        format!("{CB_FILTER_RANGE_SET_PREFIX}{field}:{from}:{to}"),
                    )
                })
                .collect()
        })
        .collect();

    // Trailing row: "no filter" + back.
    let no_filter_label = format!("{}Без фильтра", mark(0, 0));
    rows.push(vec![
        InlineKeyboardButton::callback(
            no_filter_label,
            format!("{CB_FILTER_RANGE_SET_PREFIX}{field}:0:0"),
        ),
        InlineKeyboardButton::callback("↩️ Назад", CB_FILTER_MENU),
    ]);
    InlineKeyboardMarkup::new(rows)
}

/// Model picker keyboard for a specific brand. Same multi-select shape as
/// chassis (✓/⬜ prefixes, toggle callbacks, Save/Back row).
///
/// Navigation (#6): the models picker is the one screen that logically hangs
/// *under* another picker — you got here via a brand. So "back" returns to
/// the brand picker (to fix a wrong brand without bouncing through the
/// menu), and a separate "menu" button keeps the top level one tap away.
/// Both leave without saving; the draft is discarded on either path.
///
/// `brand_slug` is captured into nothing here — the catalog lookup happens
/// at the call site; we only need the per-brand model list to render.
pub(super) fn model_picker_keyboard(
    models: &[(&'static str, &'static str)],
    selected: &[String],
) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = models
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(slug, display)| {
                    let checked = selected.iter().any(|s| s == slug);
                    let prefix = if checked { "✓ " } else { "⬜ " };
                    InlineKeyboardButton::callback(
                        format!("{prefix}{display}"),
                        format!("{CB_FILTER_MODELS_TOGGLE_PREFIX}{slug}"),
                    )
                })
                .collect()
        })
        .collect();
    rows.push(vec![InlineKeyboardButton::callback(
        "💾 Сохранить",
        CB_FILTER_MODELS_SAVE,
    )]);
    rows.push(vec![
        InlineKeyboardButton::callback("↩️ К маркам (без сохранения)", CB_FILTER_BRAND_PICKER),
        InlineKeyboardButton::callback("🏠 В меню", CB_FILTER_MENU),
    ]);
    InlineKeyboardMarkup::new(rows)
}

/// Chassis picker keyboard: a 2-wide grid of body-type buttons with a check
/// mark for items currently in `selected`, plus a Save / Back row at the
/// bottom.
///
/// Rebuilt fresh on every render — there's no clever caching. With 6 items
/// it costs nothing.
pub(super) fn chassis_picker_keyboard(selected: &[u32]) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = CHASSIS
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(code, display)| {
                    // ✓ / ⬜ prefix communicates checked state. The `code` is
                    // baked into callback_data so the handler doesn't need to
                    // look up the label-to-code mapping.
                    let prefix = if selected.contains(code) {
                        "✓ "
                    } else {
                        "⬜ "
                    };
                    InlineKeyboardButton::callback(
                        format!("{prefix}{display}"),
                        format!("{CB_FILTER_CHASSIS_TOGGLE_PREFIX}{code}"),
                    )
                })
                .collect()
        })
        .collect();
    rows.push(vec![
        InlineKeyboardButton::callback("💾 Сохранить", CB_FILTER_CHASSIS_SAVE),
        InlineKeyboardButton::callback("↩️ Назад (без сохранения)", CB_FILTER_MENU),
    ]);
    InlineKeyboardMarkup::new(rows)
}

/// Brand picker: a 4-wide grid of brand buttons plus "skip" and "back".
///
/// `chunks(4)` cleanly groups the 20 brands into 5 rows of 4. If the list
/// ever grows non-multiple-of-4, the final row will be short — TG handles
/// that fine.
pub(super) fn brand_picker_keyboard() -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = BRANDS
        .chunks(4)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(slug, display)| {
                    InlineKeyboardButton::callback(
                        *display,
                        format!("{CB_FILTER_BRAND_SET_PREFIX}{slug}"),
                    )
                })
                .collect()
        })
        .collect();
    rows.push(vec![
        InlineKeyboardButton::callback("⏭ Без фильтра", CB_FILTER_BRAND_CLEAR),
        InlineKeyboardButton::callback("💬 Ввести вручную", CB_FILTER_BRAND_CUSTOM_HINT),
    ]);
    rows.push(vec![InlineKeyboardButton::callback(
        "↩️ Назад",
        CB_FILTER_MENU,
    )]);
    InlineKeyboardMarkup::new(rows)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::super::catalog::{PRICE_RANGES, YEAR_RANGES};
    use super::*;
    use teloxide::types::InlineKeyboardButtonKind;

    /// Extracts the callback data of a button (all our buttons are
    /// callback-kind; anything else is a bug in the builder).
    fn data(btn: &InlineKeyboardButton) -> &str {
        match &btn.kind {
            InlineKeyboardButtonKind::CallbackData(d) => d,
            other => panic!("expected callback button, got {other:?}"),
        }
    }

    fn all_buttons(kb: &InlineKeyboardMarkup) -> impl Iterator<Item = &InlineKeyboardButton> {
        kb.inline_keyboard.iter().flatten()
    }

    #[test]
    fn every_keyboard_stays_under_telegram_callback_data_cap() {
        // Telegram rejects callback data over 64 bytes; catch a too-long
        // slug or prefix at test time rather than as a runtime 400.
        let keyboards = [
            filter_menu_keyboard(),
            brand_picker_keyboard(),
            chassis_picker_keyboard(&[2634]),
            interval_picker_keyboard(600),
            reset_confirm_keyboard(),
            range_picker_keyboard("price", PRICE_RANGES, (None, None)),
            range_picker_keyboard("year", YEAR_RANGES, (None, None)),
            model_picker_keyboard(
                super::super::catalog::models_for_brand("mini").unwrap(),
                &[],
            ),
        ];
        for kb in &keyboards {
            for btn in all_buttons(kb) {
                assert!(data(btn).len() <= 64, "too long: {}", data(btn));
            }
        }
    }

    #[test]
    fn interval_buttons_round_trip_through_the_callback_parser() {
        // Same parse the handler does: strip prefix, parse u64.
        let kb = interval_picker_keyboard(600);
        let parsed: Vec<u64> = all_buttons(&kb)
            .filter_map(|b| data(b).strip_prefix(CB_FILTER_INTERVAL_SET_PREFIX))
            .map(|tail| tail.parse::<u64>().unwrap())
            .collect();
        let presets: Vec<u64> = super::super::catalog::INTERVAL_PRESETS
            .iter()
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(parsed, presets);
        // Current value is marked, exactly once.
        let marked = all_buttons(&kb)
            .filter(|b| b.text.starts_with("✓ "))
            .count();
        assert_eq!(marked, 1);
    }

    #[test]
    fn range_buttons_round_trip_field_from_to() {
        // Handler-side parse: strip CB_FILTER_RANGE_SET_PREFIX, splitn(3, ':').
        let kb = range_picker_keyboard("price", PRICE_RANGES, (Some(5_000), Some(10_000)));
        let mut seen = Vec::new();
        for btn in all_buttons(&kb) {
            let Some(tail) = data(btn).strip_prefix(CB_FILTER_RANGE_SET_PREFIX) else {
                continue; // the Back button
            };
            let parts: Vec<&str> = tail.splitn(3, ':').collect();
            let [field, from, to] = parts.as_slice() else {
                panic!("malformed: {tail}");
            };
            assert_eq!(*field, "price");
            seen.push((from.parse::<u32>().unwrap(), to.parse::<u32>().unwrap()));
        }
        // All presets plus the trailing "no filter" (0,0).
        assert_eq!(seen.len(), PRICE_RANGES.len() + 1);
        assert_eq!(seen.last(), Some(&(0, 0)));
        // The current range gets the check mark.
        let marked: Vec<&str> = all_buttons(&kb)
            .filter(|b| b.text.starts_with("✓ "))
            .map(|b| b.text.as_str())
            .collect();
        assert_eq!(marked, ["✓ 5–10 000 €"]);
    }

    #[test]
    fn chassis_buttons_round_trip_codes_and_show_selection() {
        let kb = chassis_picker_keyboard(&[2632]);
        for btn in all_buttons(&kb) {
            let Some(tail) = data(btn).strip_prefix(CB_FILTER_CHASSIS_TOGGLE_PREFIX) else {
                continue; // Save / Back row
            };
            let code = tail.parse::<u32>().unwrap();
            let checked = btn.text.starts_with("✓ ");
            assert_eq!(checked, code == 2632, "{}", btn.text);
        }
    }

    #[test]
    fn model_buttons_round_trip_slugs_and_show_selection() {
        let models = super::super::catalog::models_for_brand("mini").unwrap();
        let kb = model_picker_keyboard(models, &["cooper".to_string()]);
        let mut toggles = 0;
        for btn in all_buttons(&kb) {
            let Some(slug) = data(btn).strip_prefix(CB_FILTER_MODELS_TOGGLE_PREFIX) else {
                continue;
            };
            toggles += 1;
            assert!(models.iter().any(|(s, _)| *s == slug), "{slug}");
            let checked = btn.text.starts_with("✓ ");
            assert_eq!(checked, slug == "cooper", "{}", btn.text);
        }
        assert_eq!(toggles, models.len());
    }

    #[test]
    fn model_picker_back_goes_to_brand_picker_with_menu_still_reachable() {
        // #6: models hang under a brand, so "back" = brand picker; the menu
        // must stay one tap away via its own button.
        let models = super::super::catalog::models_for_brand("mini").unwrap();
        let kb = model_picker_keyboard(models, &[]);
        let datas: Vec<&str> = all_buttons(&kb).map(data).collect();
        assert!(datas.contains(&CB_FILTER_BRAND_PICKER), "{datas:?}");
        assert!(datas.contains(&CB_FILTER_MENU), "{datas:?}");
        assert!(datas.contains(&CB_FILTER_MODELS_SAVE), "{datas:?}");
    }

    #[test]
    fn menu_spawned_screens_go_back_to_their_spawner_the_menu() {
        // Every picker opened from the top menu must offer a direct way back
        // to it (#6) — for the confirmation screen that's the Cancel button.
        let keyboards = [
            brand_picker_keyboard(),
            chassis_picker_keyboard(&[]),
            interval_picker_keyboard(600),
            reset_confirm_keyboard(),
            range_picker_keyboard("price", PRICE_RANGES, (None, None)),
            range_picker_keyboard("year", YEAR_RANGES, (None, None)),
        ];
        for kb in &keyboards {
            assert!(
                all_buttons(kb).any(|b| data(b) == CB_FILTER_MENU),
                "keyboard without a way back to the menu: {kb:?}"
            );
        }
    }

    #[test]
    fn brand_picker_covers_the_whole_catalog() {
        let kb = brand_picker_keyboard();
        let slugs: Vec<&str> = all_buttons(&kb)
            .filter_map(|b| data(b).strip_prefix(CB_FILTER_BRAND_SET_PREFIX))
            .collect();
        let expected: Vec<&str> = BRANDS.iter().map(|(s, _)| *s).collect();
        assert_eq!(slugs, expected);
    }
}
