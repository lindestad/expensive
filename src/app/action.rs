use crossterm::event::{KeyCode, KeyModifiers};

use crate::time_window::CalendarScale;

use super::View;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Action {
    Quit,
    Back,
    OpenCalendar,
    NextTab,
    PreviousTab,
    Refresh,
    RebuildIndex,
    ToggleGraphMetric,
    OpenScopePicker,
    OpenSelectedPeriod,
    MoveCalendar(i32),
}

impl Action {
    pub(super) fn from_key(
        code: KeyCode,
        modifiers: KeyModifiers,
        view: View,
        scale: CalendarScale,
    ) -> Option<Self> {
        match code {
            KeyCode::Char('q') => Some(Self::Quit),
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Some(Self::Quit),
            KeyCode::Esc => Some(Self::Back),
            KeyCode::Char('c') if view == View::Dashboard => Some(Self::OpenCalendar),
            KeyCode::Tab => Some(Self::NextTab),
            KeyCode::BackTab => Some(Self::PreviousTab),
            KeyCode::Char('R') => Some(Self::RebuildIndex),
            KeyCode::Char('r') => Some(Self::Refresh),
            KeyCode::Char('g') => Some(Self::ToggleGraphMetric),
            KeyCode::Char('p') => Some(Self::OpenScopePicker),
            KeyCode::Enter | KeyCode::Char(' ') if view == View::CalendarOverview => {
                Some(Self::OpenSelectedPeriod)
            }
            _ => calendar_move(code, view, scale).map(Self::MoveCalendar),
        }
    }
}

fn calendar_move(code: KeyCode, view: View, scale: CalendarScale) -> Option<i32> {
    match view {
        View::Dashboard => None,
        View::CalendarOverview => match code {
            KeyCode::Left | KeyCode::Char('h') => Some(-1),
            KeyCode::Right | KeyCode::Char('l') => Some(1),
            KeyCode::Up | KeyCode::Char('k') => Some(-overview_columns(scale)),
            KeyCode::Down | KeyCode::Char('j') => Some(overview_columns(scale)),
            _ => None,
        },
        View::CalendarDetail => match code {
            KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => Some(-1),
            KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => Some(1),
            _ => None,
        },
    }
}

fn overview_columns(scale: CalendarScale) -> i32 {
    match scale {
        CalendarScale::Day => 7,
        CalendarScale::Week => 4,
        CalendarScale::Month => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_calendar_navigation_to_domain_steps() {
        assert_eq!(
            Action::from_key(
                KeyCode::Up,
                KeyModifiers::NONE,
                View::CalendarOverview,
                CalendarScale::Day,
            ),
            Some(Action::MoveCalendar(-7))
        );
        assert_eq!(
            Action::from_key(
                KeyCode::Right,
                KeyModifiers::NONE,
                View::CalendarDetail,
                CalendarScale::Month,
            ),
            Some(Action::MoveCalendar(1))
        );
    }

    #[test]
    fn ignores_calendar_keys_on_dashboard() {
        assert_eq!(
            Action::from_key(
                KeyCode::Left,
                KeyModifiers::NONE,
                View::Dashboard,
                CalendarScale::Day,
            ),
            None
        );
    }

    #[test]
    fn distinguishes_incremental_and_full_refreshes() {
        assert_eq!(
            Action::from_key(
                KeyCode::Char('r'),
                KeyModifiers::NONE,
                View::Dashboard,
                CalendarScale::Day,
            ),
            Some(Action::Refresh)
        );
        assert_eq!(
            Action::from_key(
                KeyCode::Char('R'),
                KeyModifiers::SHIFT,
                View::Dashboard,
                CalendarScale::Day,
            ),
            Some(Action::RebuildIndex)
        );
    }
}
