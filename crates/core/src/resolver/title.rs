//! A page title that names one episode of a series — `《劇名》第6集`,
//! `劇名 第6話`, `Show - Episode 6`, `Show EP06`, `Show S01E06` — split into
//! the series and the episode, so episodes added one page at a time land in
//! one folder, numbered. The fallbacks (`page`, `yt-dlp`) see one page at a
//! time and would otherwise make a folder per episode. Anything else is
//! left whole.

#[derive(Debug, PartialEq)]
pub(crate) struct Episode {
    pub series: String,
    pub index: i64,
    /// `第6集`, `Episode 6`, `S01E06`.
    pub label: String,
}

pub(crate) fn split_episode(title: &str) -> Option<Episode> {
    cjk(title).or_else(|| latin(title))
}

/// `…第<digits><集|話|话|回>`.
fn cjk(title: &str) -> Option<Episode> {
    for (at, _) in title.match_indices('第') {
        let after = &title[at + '第'.len_utf8()..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let unit = after[digits.len()..].chars().next();
        if digits.is_empty() || !matches!(unit, Some('集' | '話' | '话' | '回')) {
            continue;
        }
        let index = digits.parse().ok()?;
        let series = clean(&title[..at])?;
        return Some(Episode {
            series,
            index,
            label: format!("第{index}{}", unit.unwrap_or('集')),
        });
    }
    None
}

/// `S01E06`, `Episode 6`, `Ep. 6`, `EP06`, each starting a word.
fn latin(title: &str) -> Option<Episode> {
    let lower = title.to_ascii_lowercase();
    let starts_word = |i: usize| {
        lower[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric())
    };
    let number = |s: &str| -> Option<(i64, usize)> {
        let s_trim = s.trim_start_matches([' ', '.']);
        let skipped = s.len() - s_trim.len();
        let digits: String = s_trim.chars().take_while(char::is_ascii_digit).collect();
        let end = skipped + digits.len();
        let ends_word = s[end..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        (!digits.is_empty() && ends_word).then(|| Some((digits.parse().ok()?, end)))?
    };
    for (i, _) in lower.char_indices().filter(|(i, _)| starts_word(*i)) {
        let rest = &lower[i..];
        // S01E06
        if let Some(r) = rest.strip_prefix('s') {
            let season: String = r.chars().take_while(char::is_ascii_digit).collect();
            if !season.is_empty()
                && let Some(r) = r[season.len()..].strip_prefix('e')
                && let Some((index, end)) = number(r)
            {
                // `s` + season + `e` + episode, as written, in capitals.
                let len = 1 + season.len() + 1 + end;
                let label = title[i..i + len].to_ascii_uppercase();
                return Some(Episode {
                    series: clean(&title[..i])?,
                    index,
                    label,
                });
            }
        }
        for word in ["episode", "ep"] {
            if let Some(r) = rest.strip_prefix(word)
                && let Some((index, _)) = number(r)
            {
                return Some(Episode {
                    series: clean(&title[..i])?,
                    index,
                    label: format!("Episode {index}"),
                });
            }
        }
    }
    None
}

/// The series name without the brackets and separators around it.
fn clean(s: &str) -> Option<String> {
    let s = s.trim_matches(|c: char| c.is_whitespace() || "-–—|:：·,，_".contains(c));
    let s = [('《', '》'), ('「', '」'), ('【', '】'), ('『', '』')]
        .iter()
        .find_map(|(open, close)| s.strip_prefix(*open)?.strip_suffix(*close))
        .unwrap_or(s)
        .trim();
    (!s.is_empty()).then(|| s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(series: &str, index: i64, label: &str) -> Option<Episode> {
        Some(Episode {
            series: series.into(),
            index,
            label: label.into(),
        })
    }

    #[test]
    fn episode_titles_split_into_series_and_number() {
        assert_eq!(
            split_episode("《網調我的冰山媽媽》第6集"),
            ep("網調我的冰山媽媽", 6, "第6集")
        );
        assert_eq!(
            split_episode("《劇名》第6集 (1)"),
            ep("劇名", 6, "第6集"),
            "yt-dlp's suffix"
        );
        assert_eq!(split_episode("劇名 第12話"), ep("劇名", 12, "第12話"));
        assert_eq!(
            split_episode("Some Show - Episode 6"),
            ep("Some Show", 6, "Episode 6")
        );
        assert_eq!(
            split_episode("Some Show EP06"),
            ep("Some Show", 6, "Episode 6")
        );
        assert_eq!(
            split_episode("Some Show Ep. 7 | Site"),
            ep("Some Show", 7, "Episode 7")
        );
        assert_eq!(
            split_episode("Some Show S01E06"),
            ep("Some Show", 6, "S01E06")
        );
    }

    #[test]
    fn other_titles_are_left_whole() {
        for t in [
            "A film",
            "第一集",
            "Deep Sleep 4",
            "Keep2 going",
            "《劇名》",
            "第6集",
            "Steps 3",
        ] {
            assert_eq!(split_episode(t), None, "{t}");
        }
    }
}
