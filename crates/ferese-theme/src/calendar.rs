//! Date/time labels and a calendar grid; callers own navigation and popup behavior.
use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::{Alignment, Background, Border, Length};
use cosmic::widget::{column, container, row};
use cosmic::{Element, theme};
use jiff::Zoned;

pub fn format_bar_time(now: &Zoned) -> String {
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sept", "oct", "nov", "dec",
    ];
    let hour = now.hour();
    let hour12 = if hour % 12 == 0 { 12 } else { hour % 12 };
    format!(
        "{} {}, {}:{:02} {}",
        now.day(),
        months[(now.month() - 1) as usize],
        hour12,
        now.minute(),
        if hour < 12 { "am" } else { "pm" }
    )
}

pub fn stacked_digits(label: &str) -> Option<[String; 2]> {
    let mut parts = label.split_whitespace();
    let time = parts.next()?;

    if let Some(period) = parts.next()
        && !period.eq_ignore_ascii_case("am")
        && !period.eq_ignore_ascii_case("pm")
    {
        return None;
    }

    if parts.next().is_some() {
        return None;
    }

    let (hour, minute) = time.split_once(':')?;
    if !(1..=2).contains(&hour.len())
        || minute.len() != 2
        || !hour.bytes().chain(minute.bytes()).all(|digit| digit.is_ascii_digit())
    {
        return None;
    }

    Some([format!("{hour:0>2}"), minute.to_owned()])
}

pub fn month(today: jiff::civil::Date, offset: i32) -> (jiff::civil::Date, usize, usize) {
    let month_index = i32::from(today.year()) * 12 + i32::from(today.month()) - 1 + offset;
    let year = month_index.div_euclid(12) as i16;
    let month = (month_index.rem_euclid(12) + 1) as i8;
    let first = jiff::civil::Date::new(year, month, 1).expect("calendar month is in range");
    let start = usize::try_from(first.weekday().since(jiff::civil::Weekday::Monday)).unwrap_or(0);
    let days = usize::try_from(first.days_in_month()).unwrap_or(31);
    (first, start, days)
}

pub fn grid<'a, M: Clone + 'a>(
    today: jiff::civil::Date,
    offset: i32,
    palette: crate::Palette,
    font: cosmic::font::Font,
    opacity: f32,
    navigation: [Element<'a, M>; 3],
) -> Element<'a, M> {
    let months = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let (first, start, days) = month(today, offset);
    let year = first.year();
    let month = first.month();
    let primary = palette.text.scale_alpha(opacity);
    let muted = palette.muted.scale_alpha(opacity);
    let [previous, today_button, next] = navigation;
    let header = row![
        crate::text(format!("{} {year}", months[(month - 1) as usize]), font)
            .size(15)
            .width(Length::Fill),
        previous,
        today_button,
        next,
    ]
    .align_y(Alignment::Center)
    .spacing(2);
    let mut grid = column::with_capacity(8).spacing(3).push(header);
    let mut weekdays = row::with_capacity(7).spacing(3);
    for label in ["M", "T", "W", "T", "F", "S", "S"] {
        weekdays = weekdays.push(
            container(crate::text(label, font).size(11).class(theme::Text::Color(muted)))
                .width(30)
                .center_x(30),
        );
    }
    grid = grid.push(weekdays);
    for week in 0..(start + days).div_ceil(7) {
        let mut dates = row::with_capacity(7).spacing(3);
        for weekday in 0..7 {
            let cell = week * 7 + weekday;
            let day = cell.checked_sub(start).map(|day| day + 1).filter(|day| *day <= days);
            let is_today = year == today.year() && month == today.month() && day == Some(today.day() as usize);
            let label = day.map_or(String::new(), |day| day.to_string());
            dates = dates.push(
                container(crate::text(label, font).size(12).class(theme::Text::Color(if is_today {
                    primary
                } else {
                    muted
                })))
                .width(30)
                .height(25)
                .center_x(30)
                .center_y(25)
                .class(theme::Container::custom(move |_| container::Style {
                    background: is_today.then_some(Background::Color(palette.accent.scale_alpha(0.2 * opacity))),
                    border: Border {
                        shape: BorderShape::Continuous,
                        radius: palette.radius.min(12.).into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })),
            );
        }
        grid = grid.push(dates);
    }
    grid.into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn pixel_clock_stacks_only_hour_and_minute_formats() {
        assert_eq!(super::stacked_digits("2:45 pm"), Some(["02".into(), "45".into()]));
        assert_eq!(super::stacked_digits("14:45"), Some(["14".into(), "45".into()]));
        assert!(super::stacked_digits("14:45:30").is_none());
        assert!(super::stacked_digits("Today 14:45").is_none());
        assert!(super::stacked_digits("14:45 pm extra").is_none());
    }

    #[test]
    fn clock_uses_lowercase_date_and_twelve_hour_time() {
        for (stamp, expected) in [
            ("2026-09-28T21:32:00+01:00[Africa/Lagos]", "28 sept, 9:32 pm"),
            ("2026-09-28T00:05:00+01:00[Africa/Lagos]", "28 sept, 12:05 am"),
            ("2026-09-28T12:00:00+01:00[Africa/Lagos]", "28 sept, 12:00 pm"),
        ] {
            assert_eq!(super::format_bar_time(&stamp.parse().unwrap()), expected);
        }
    }

    #[test]
    fn calendar_aligns_leap_months_and_year_boundaries() {
        let february = jiff::civil::Date::new(2024, 2, 15).unwrap();
        let (first, start, days) = super::month(february, 0);
        assert_eq!((first.year(), first.month(), start, days), (2024, 2, 3, 29));
        let (first, start, days) = super::month(february, -2);
        assert_eq!((first.year(), first.month(), start, days), (2023, 12, 4, 31));
        let june = jiff::civil::Date::new(2025, 6, 1).unwrap();
        let (_, start, days) = super::month(june, 0);
        assert_eq!((start, days), (6, 30));
    }
}
