//! Clack-style prompts for the config wizard, drawn on stderr.

use std::fmt::Display;
use std::io;

use console::{Key, StyledObject, Term, measure_text_width, style, truncate_str};

type Validator = Box<dyn Fn(&String) -> Result<(), String>>;

fn paint<D>(text: D) -> StyledObject<D> {
    style(text).for_stderr()
}

fn muted<D>(text: D) -> StyledObject<D> {
    paint(text).black().bright()
}

enum Phase {
    Active,
    Error(String),
    Submit,
    Cancel,
}

impl Phase {
    fn bar<D>(&self, text: D) -> StyledObject<D> {
        match self {
            Phase::Active => paint(text).cyan(),
            Phase::Error(_) => paint(text).yellow(),
            Phase::Submit => muted(text),
            Phase::Cancel => paint(text).red(),
        }
    }

    fn symbol(&self) -> StyledObject<&'static str> {
        match self {
            Phase::Active => self.bar("◆"),
            Phase::Error(_) => self.bar("▲"),
            Phase::Submit => paint("◇").green(),
            Phase::Cancel => self.bar("■"),
        }
    }

    /// How a prompt shows its answer once it is no longer being edited.
    fn settled(&self, answer: &str) -> Option<String> {
        match self {
            Phase::Submit => Some(paint(answer).dim().to_string()),
            Phase::Cancel => Some(paint(answer).dim().strikethrough().to_string()),
            _ => None,
        }
    }
}

trait Widget {
    type Output;

    fn key(&mut self, key: Key);
    fn submit(&mut self) -> Result<Self::Output, String>;
    fn body(&self, phase: &Phase) -> Vec<String>;
}

pub struct Select<T> {
    question: String,
    items: Vec<(T, String, String)>,
    cursor: usize,
    top: usize,
    visible: usize,
}

pub fn select<T: Clone + PartialEq>(question: impl Display) -> Select<T> {
    Select {
        question: question.to_string(),
        items: Vec::new(),
        cursor: 0,
        top: 0,
        visible: usize::MAX,
    }
}

impl<T: Clone + PartialEq> Select<T> {
    pub fn item(mut self, value: T, label: impl Display, hint: impl Display) -> Self {
        self.items
            .push((value, label.to_string(), hint.to_string()));
        self
    }

    /// Starts on the item holding `value`; call it after the items are added.
    pub fn initial_value(mut self, value: T) -> Self {
        self.cursor = self
            .items
            .iter()
            .position(|(v, _, _)| *v == value)
            .unwrap_or(0);
        self
    }

    pub fn interact(mut self) -> io::Result<T> {
        let rows = Term::stderr().size().0 as usize;
        self.visible = rows.saturating_sub(5).max(1);
        self.scroll();
        interact(&self.question.clone(), self)
    }
}

impl<T> Select<T> {
    fn scroll(&mut self) {
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top.saturating_add(self.visible) {
            self.top = self.cursor + 1 - self.visible;
        }
    }
}

impl<T: Clone> Widget for Select<T> {
    type Output = T;

    fn key(&mut self, key: Key) {
        match key {
            Key::ArrowUp => self.cursor = self.cursor.saturating_sub(1),
            Key::ArrowDown => self.cursor = (self.cursor + 1).min(self.items.len() - 1),
            _ => {}
        }
        self.scroll();
    }

    fn submit(&mut self) -> Result<T, String> {
        Ok(self.items[self.cursor].0.clone())
    }

    fn body(&self, phase: &Phase) -> Vec<String> {
        if let Some(answer) = phase.settled(&self.items[self.cursor].1) {
            return vec![answer];
        }
        let end = self.top.saturating_add(self.visible).min(self.items.len());
        let mut lines = Vec::new();
        if self.top > 0 {
            lines.push(paint("...").dim().to_string());
        }
        for (i, (_, label, hint)) in self.items[self.top..end].iter().enumerate() {
            lines.push(if self.top + i != self.cursor {
                format!("{} {}", paint("○").dim(), paint(label).dim())
            } else if hint.is_empty() {
                format!("{} {label}", paint("●").green())
            } else {
                let hint = paint(format!("({hint})")).dim();
                format!("{} {label} {hint}", paint("●").green())
            });
        }
        if end < self.items.len() {
            lines.push(paint("...").dim().to_string());
        }
        lines
    }
}

pub struct Input {
    question: String,
    value: String,
    placeholder: Option<String>,
    default: Option<String>,
    validator: Option<Validator>,
}

pub fn input(question: impl Display) -> Input {
    Input {
        question: question.to_string(),
        value: String::new(),
        placeholder: None,
        default: None,
        validator: None,
    }
}

impl Input {
    pub fn placeholder(mut self, text: &str) -> Self {
        self.placeholder = Some(text.to_string());
        self
    }

    /// What an empty answer means; shown as the placeholder unless one is set.
    pub fn default_input(mut self, value: &str) -> Self {
        self.default = Some(value.to_string());
        self
    }

    pub fn validate<E: Display>(
        mut self,
        check: impl Fn(&String) -> Result<(), E> + 'static,
    ) -> Self {
        self.validator = Some(Box::new(move |s| check(s).map_err(|e| e.to_string())));
        self
    }

    pub fn interact(self) -> io::Result<String> {
        interact(&self.question.clone(), self)
    }
}

impl Widget for Input {
    type Output = String;

    fn key(&mut self, key: Key) {
        match key {
            Key::Char(c) if !c.is_control() => self.value.push(c),
            Key::Backspace => {
                self.value.pop();
            }
            _ => {}
        }
    }

    fn submit(&mut self) -> Result<String, String> {
        if self.value.is_empty() {
            self.value = self.default.clone().ok_or("Input required")?;
        }
        if let Some(validator) = &self.validator {
            validator(&self.value)?;
        }
        Ok(self.value.clone())
    }

    fn body(&self, phase: &Phase) -> Vec<String> {
        if let Some(answer) = phase.settled(&self.value) {
            return vec![answer];
        }
        let hint = self.placeholder.as_ref().or(self.default.as_ref());
        vec![match hint {
            _ if !self.value.is_empty() => format!("{}{}", self.value, paint(" ").reverse()),
            Some(hint) if !hint.is_empty() => {
                let (first, rest) = hint.split_at(hint.chars().next().map_or(0, char::len_utf8));
                format!("{}{}", paint(first).dim().reverse(), paint(rest).dim())
            }
            _ => paint(" ").reverse().to_string(),
        }]
    }
}

pub struct Confirm {
    question: String,
    yes: bool,
}

pub fn confirm(question: impl Display) -> Confirm {
    Confirm {
        question: question.to_string(),
        yes: false,
    }
}

impl Confirm {
    pub fn initial_value(mut self, yes: bool) -> Self {
        self.yes = yes;
        self
    }

    pub fn interact(self) -> io::Result<bool> {
        interact(&self.question.clone(), self)
    }
}

impl Widget for Confirm {
    type Output = bool;

    fn key(&mut self, key: Key) {
        match key {
            Key::ArrowLeft | Key::Char('y' | 'Y') => self.yes = true,
            Key::ArrowRight | Key::Char('n' | 'N') => self.yes = false,
            _ => {}
        }
    }

    fn submit(&mut self) -> Result<bool, String> {
        Ok(self.yes)
    }

    fn body(&self, phase: &Phase) -> Vec<String> {
        if let Some(answer) = phase.settled(if self.yes { "Yes" } else { "No" }) {
            return vec![answer];
        }
        let radio = |on: bool, label: &str| {
            if on {
                format!("{} {label}", paint("●").green())
            } else {
                format!("{} {}", paint("○").dim(), paint(label).dim())
            }
        };
        vec![format!(
            "{}{}{}",
            radio(self.yes, "Yes"),
            paint(" / ").dim(),
            radio(!self.yes, "No")
        )]
    }
}

fn frame(question: &str, widget: &impl Widget, phase: &Phase) -> Vec<String> {
    let mut lines = vec![format!("{}  {question}", phase.symbol())];
    for line in widget.body(phase) {
        lines.push(format!("{}  {line}", phase.bar("│")));
    }
    lines.push(match phase {
        Phase::Active => phase.bar("└").to_string(),
        Phase::Error(message) => phase.bar(format!("└  {message}")).to_string(),
        Phase::Submit => phase.bar("│").to_string(),
        Phase::Cancel => phase.bar("└  Operation cancelled.").to_string(),
    });
    lines
}

/// Redraws over the previous frame. Lines are cut to the terminal's width,
/// because one that wrapped would throw off how far up the next redraw reaches.
fn draw(term: &Term, previous: usize, lines: &[String]) -> io::Result<usize> {
    term.clear_last_lines(previous)?;
    let width = (term.size().1 as usize).saturating_sub(1);
    for line in lines {
        term.write_line(&truncate_str(line, width, "…"))?;
    }
    term.flush()?;
    Ok(lines.len())
}

struct HiddenCursor<'a>(&'a Term);

impl Drop for HiddenCursor<'_> {
    fn drop(&mut self) {
        let _ = self.0.show_cursor();
        let _ = self.0.flush();
    }
}

fn interact<W: Widget>(question: &str, mut widget: W) -> io::Result<W::Output> {
    let term = Term::buffered_stderr();
    term.hide_cursor()?;
    let _cursor = HiddenCursor(&term);

    let mut phase = Phase::Active;
    let mut drawn = 0;
    let outcome = loop {
        drawn = draw(&term, drawn, &frame(question, &widget, &phase))?;
        phase = Phase::Active;
        match term.read_key_raw()? {
            Key::Enter => match widget.submit() {
                Ok(value) => break Some(value),
                Err(message) => phase = Phase::Error(message),
            },
            Key::Escape | Key::CtrlC => break None,
            key => widget.key(key),
        }
    };
    let phase = if outcome.is_some() {
        Phase::Submit
    } else {
        Phase::Cancel
    };
    draw(&term, drawn, &frame(question, &widget, &phase))?;
    outcome.ok_or_else(|| io::ErrorKind::Interrupted.into())
}

pub fn intro(title: impl Display) -> io::Result<()> {
    Term::stderr().write_line(&format!("{}  {title}\n{}", muted("┌"), muted("│")))
}

pub fn outro(message: impl Display) -> io::Result<()> {
    Term::stderr().write_line(&format!("{}  {message}\n", muted("└")))
}

pub fn outro_cancel(message: impl Display) -> io::Result<()> {
    let message = paint(message.to_string()).red();
    Term::stderr().write_line(&format!("{}  {message}\n", muted("└")))
}

pub fn remark(text: impl Display) -> io::Result<()> {
    log(muted("├"), &text.to_string())
}

pub fn warning(text: impl Display) -> io::Result<()> {
    log(paint("▲").yellow(), &text.to_string())
}

fn log(symbol: StyledObject<&str>, text: &str) -> io::Result<()> {
    let mut lines = text.lines();
    let mut out = format!("{symbol}  {}\n", lines.next().unwrap_or_default());
    for line in lines {
        out += &format!("{}  {line}\n", muted("│"));
    }
    Term::stderr().write_line(&format!("{out}{}", muted("│")))
}

pub fn note(title: impl Display, message: impl Display) -> io::Result<()> {
    Term::stderr().write_str(&note_box(&title.to_string(), &message.to_string()))
}

fn note_box(title: &str, message: &str) -> String {
    let message = format!("\n{message}\n");
    let header = format!("  {title} ");
    let width = message
        .split('\n')
        .map(|line| measure_text_width(line) + 2)
        .max()
        .unwrap_or(0)
        .max(measure_text_width(&header));

    let rule = |n: usize| "─".repeat(n);
    let mut out = format!(
        "{}{header}{}\n",
        paint("◇").green(),
        muted(format!(
            "{}╮",
            rule(2 + width - measure_text_width(&header))
        ))
    );
    for line in message.split('\n') {
        let pad = " ".repeat(width - measure_text_width(line));
        out += &format!("{}  {}{pad}{}\n", muted("│"), paint(line).dim(), muted("│"));
    }
    out + &format!(
        "{}\n{}\n",
        muted(format!("├{}╯", rule(width + 2))),
        muted("│")
    )
}

#[cfg(test)]
mod tests {
    use console::strip_ansi_codes;

    use super::*;

    fn plain(lines: Vec<String>) -> Vec<String> {
        lines
            .iter()
            .map(|l| strip_ansi_codes(l).into_owned())
            .collect()
    }

    #[test]
    fn select_moves_within_its_items_and_scrolls_a_short_window() {
        let mut menu = select("Pick")
            .item(1, "one", "first")
            .item(2, "two", "")
            .item(3, "three", "")
            .initial_value(2);
        menu.visible = 2;
        assert_eq!(plain(menu.body(&Phase::Active)), ["○ one", "● two", "..."]);

        menu.key(Key::ArrowDown);
        menu.key(Key::ArrowDown);
        assert_eq!(
            plain(menu.body(&Phase::Active)),
            ["...", "○ two", "● three"]
        );
        assert_eq!(menu.submit(), Ok(3));
        assert_eq!(plain(menu.body(&Phase::Submit)), ["three"]);

        for _ in 0..3 {
            menu.key(Key::ArrowUp);
        }
        assert_eq!(
            plain(menu.body(&Phase::Active)),
            ["● one (first)", "○ two", "..."]
        );
    }

    #[test]
    fn input_falls_back_to_its_default_and_holds_invalid_answers() {
        let ask = || {
            input("How long?")
                .default_input("1h")
                .validate(|s: &String| if s == "soon" { Err("no") } else { Ok(()) })
        };

        let mut prompt = ask();
        assert_eq!(plain(prompt.body(&Phase::Active)), ["1h"]);
        assert_eq!(prompt.submit(), Ok("1h".to_string()));

        let mut prompt = ask();
        for c in "soon!".chars() {
            prompt.key(Key::Char(c));
        }
        prompt.key(Key::Backspace);
        assert_eq!(plain(prompt.body(&Phase::Active)), ["soon "]);
        assert_eq!(prompt.submit(), Err("no".to_string()));

        assert_eq!(input("Name?").submit(), Err("Input required".to_string()));
    }

    #[test]
    fn note_box_lines_up_around_styled_text() {
        let message = format!(
            "{} value\nlonger line here",
            style("label").dim().force_styling(true)
        );
        let note = strip_ansi_codes(&note_box("Title", &message)).into_owned();
        let lines: Vec<&str> = note.lines().collect();
        assert_eq!(
            lines,
            [
                "◇  Title ────────────╮",
                "│                    │",
                "│  label value       │",
                "│  longer line here  │",
                "│                    │",
                "├────────────────────╯",
                "│",
            ]
        );
    }
}
