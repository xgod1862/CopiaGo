use std::{
    io::{self, BufRead},
    sync::mpsc,
    thread,
};

use copiago_core::clip::FileOffer;

use super::window::WindowWaker;

#[derive(Default)]
pub(super) struct PendingOffer {
    next_id: u64,
    pending: Option<(u64, FileOffer)>,
}

impl PendingOffer {
    pub(super) fn set(&mut self, offer: FileOffer) -> u64 {
        self.next_id += 1;
        self.pending = Some((self.next_id, offer));
        self.next_id
    }

    pub(super) fn take(&mut self, id: u64) -> Option<FileOffer> {
        if self
            .pending
            .as_ref()
            .is_some_and(|(current, _)| *current == id)
        {
            self.pending.take().map(|(_, offer)| offer)
        } else {
            None
        }
    }

    pub(super) fn clear(&mut self) {
        self.pending = None;
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Answer {
    pub id: u64,
    pub accept: bool,
}

impl Answer {
    fn parse(line: &str) -> Option<Self> {
        let mut words = line.split_ascii_whitespace();
        let accept = match words.next()? {
            "accept" => true,
            "ignore" => false,
            _ => return None,
        };
        let id = words.next()?.parse().ok()?;
        words.next().is_none().then_some(Self { id, accept })
    }
}

/// Input can block indefinitely. The process owns this thread and can exit independently.
pub(super) fn read_answers(waker: WindowWaker) -> io::Result<mpsc::Receiver<Answer>> {
    let (send, answers) = mpsc::channel();
    thread::Builder::new()
        .name("copiago-console-input".to_owned())
        .spawn(move || {
            for line in io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if let Some(answer) = Answer::parse(&line)
                    && (send.send(answer).is_err() || !waker.file_answer())
                {
                    break;
                }
            }
        })?;
    Ok(answers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_explicit_answers_for_a_specific_offer() {
        assert_eq!(
            Answer::parse("accept 2"),
            Some(Answer {
                id: 2,
                accept: true
            })
        );
        assert_eq!(
            Answer::parse(" ignore 3\r\n"),
            Some(Answer {
                id: 3,
                accept: false
            })
        );
        for incomplete in ["yes", "accept", "accept File", "accept 2 3"] {
            assert_eq!(Answer::parse(incomplete), None);
        }
    }

    #[test]
    fn a_stale_or_repeated_answer_cannot_accept_another_file() {
        let mut pending = PendingOffer::default();
        let offer = FileOffer {
            name: "sample.pdf".to_owned(),
            uti: "com.adobe.pdf".to_owned(),
            size: 1,
            change: None,
            data: None,
        };
        let first = pending.set(offer.clone());
        let second = pending.set(offer.clone());
        assert!(pending.take(first).is_none());
        assert!(pending.take(second).is_some_and(|pending| pending == offer));
        assert!(pending.take(second).is_none());
        let third = pending.set(offer);
        pending.clear();
        assert!(pending.take(third).is_none());
    }
}
