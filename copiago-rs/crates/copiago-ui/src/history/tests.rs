use chrono::NaiveDate;
use copiago_core::i18n::message;

use super::{
    text::{fit, human_size, short_time, wrap},
    *,
};

fn history(ids: &[i64], enabled: bool) -> HistoryList {
    HistoryList {
        enabled,
        entries: ids
            .iter()
            .map(|&id| Entry {
                id,
                created: "2026-11-24T14:23".into(),
                kind: "text".into(),
                preview: b"synthetic".to_vec(),
            })
            .collect(),
    }
}

#[test]
fn newest_follows_until_a_user_selects_and_ids_survive_reflow() {
    let start = Instant::now();
    let mut page = HistoryPage::new(Language::ZhCn);
    let phone_id = |id| HistoryRowId {
        source: Source::Phone,
        id,
    };
    page.set_entries(history(&[3, 2, 1], true), history(&[12], false));
    assert_eq!(page.selected(), Some(phone_id(3)));
    page.set_entries(history(&[4, 3, 2, 1], true), history(&[12], false));
    assert_eq!(page.selected(), Some(phone_id(4)));
    assert_eq!(page.activate(HistoryTarget::Row(phone_id(2)), start), None);
    page.set_entries(history(&[5, 4, 3, 2, 1], false), history(&[12], false));
    assert_eq!(page.selected(), Some(phone_id(2)));
    let target = HistoryTarget::Action(HistoryAction::Copy(phone_id(2)));
    assert_eq!(
        page.activate(target, start),
        Some(HistoryAction::Copy(phone_id(2)))
    );
    page.set_entries(history(&[5, 4, 3, 1], false), history(&[12], false));
    assert_eq!(page.activate(target, start), None);
    assert_eq!(page.selected(), Some(phone_id(1)));
    page.activate(HistoryTarget::Tab(Source::Pc), start);
    let pc = HistoryRowId {
        source: Source::Pc,
        id: 12,
    };
    assert_eq!(page.selected(), Some(pc));
    assert_eq!(
        page.activate(HistoryTarget::Action(HistoryAction::Send(pc)), start),
        Some(HistoryAction::Send(pc))
    );
}

#[test]
fn clear_confirmation_expires_and_is_scoped_to_a_source() {
    let start = Instant::now();
    let mut page = HistoryPage::new(Language::ZhCn);
    page.set_entries(history(&[3, 2, 1], true), history(&[12], true));
    let phone = HistoryTarget::Action(HistoryAction::Clear(Source::Phone));
    let pc = HistoryTarget::Action(HistoryAction::Clear(Source::Pc));
    assert_eq!(page.activate(phone, start), None);
    assert_eq!(page.clear_deadline(), Some(start + CONFIRM_TIMEOUT));
    assert_eq!(page.activate(phone, start + CONFIRM_TIMEOUT), None);
    page.activate(HistoryTarget::Tab(Source::Pc), start);
    assert_eq!(page.clear_deadline(), None);
    assert_eq!(page.activate(phone, start), None);
    assert_eq!(page.activate(pc, start), None);
    assert_eq!(
        page.activate(pc, start + Duration::from_secs(2)),
        Some(HistoryAction::Clear(Source::Pc))
    );
    assert_eq!(page.clear_deadline(), None);
    assert_eq!(page.activate(pc, start), None);
    assert!(page.expire_confirmation(start + CONFIRM_TIMEOUT));
}

#[test]
fn history_off_and_file_offer_actions_follow_the_rendered_state() {
    let now = Instant::now();
    let mut page = HistoryPage::new(Language::En);
    page.activate(HistoryTarget::Tab(Source::Pc), now);
    let enable = HistoryAction::Enable(Source::Pc);
    assert_eq!(
        page.activate(HistoryTarget::Action(enable), now),
        Some(enable)
    );
    page.set_entries(history(&[], true), history(&[12], false));
    assert_eq!(page.activate(HistoryTarget::Action(enable), now), None);
    let mut offer = HistoryOffer {
        id: 3,
        name: "synthetic.pdf".into(),
        metadata: "PDF · 24.0 MB".into(),
        reason: message!("超过 {limit} MB，要同步到电脑吗？", limit = 20),
        busy: false,
    };
    page.set_offer(Some(offer.clone()));
    assert_eq!(page.tab(), Source::Phone);
    let accept = HistoryAction::RespondToOffer {
        id: 3,
        accept: true,
    };
    let ignore = HistoryAction::RespondToOffer {
        id: 3,
        accept: false,
    };
    assert_eq!(
        page.activate(HistoryTarget::Action(accept), now),
        Some(accept)
    );
    assert_eq!(
        page.activate(HistoryTarget::Action(ignore), now),
        Some(ignore)
    );
    offer.busy = true;
    page.set_offer(Some(offer.clone()));
    assert_eq!(page.activate(HistoryTarget::Action(accept), now), None);
    assert_eq!(page.activate(HistoryTarget::Action(ignore), now), None);
    offer.busy = false;
    offer.id += 1;
    page.set_offer(Some(offer));
    assert_eq!(page.activate(HistoryTarget::Action(accept), now), None);
}

#[test]
fn previews_keep_closing_punctuation_latin_words_and_unicode_boundaries() {
    let measure = |line: &str| Ok(line.chars().count() as f32);
    assert_eq!(fit("今天有太阳🌤", 5.0, measure).unwrap(), "今天有太…");
    assert_eq!(fit("🌤明", 2.0, measure).unwrap(), "🌤明");
    assert_eq!(
        wrap("一二三，四五六七八", 3.0, 2, measure).unwrap(),
        ["一二三，", "四五…"]
    );
    assert_eq!(
        wrap("sun red star", 6.0, 3, measure).unwrap(),
        ["sun", "red", "star"]
    );
}

#[test]
fn time_uses_local_calendar_dates_and_bilingual_yesterday() {
    let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    let zh = |created| short_time(created, today, Language::ZhCn);
    assert_eq!(zh("2026-01-01T09:47"), "09:47");
    assert_eq!(zh("2025-12-31T22:37"), "昨天 22:37");
    assert_eq!(zh("2026-01-04T12:45"), "01-04");
    assert_eq!(zh("2025-12-30T11:08"), "2025-12-30");
    assert_eq!(zh("2026-02-30T05:45"), "");
    assert_eq!(zh("2026-01-01T09:47:51+08:00"), "09:47");
    assert_eq!(
        short_time("2025-12-31T22:37", today, Language::En),
        "Yesterday 22:37"
    );
    assert_eq!(human_size(127), "127 B");
    assert_eq!(human_size(1536), "2 KB");
    assert_eq!(human_size(19 * 1024 * 1024), "19.0 MB");
}
