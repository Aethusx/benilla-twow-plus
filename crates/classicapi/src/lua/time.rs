//! `time/`: `C_Timer`, `C_DateAndTime`, `GetServerTime` and `GetTimeCached`.
//!
//! Timers run off the frame: each due callback is called once a frame, a ticker re-armed by its
//! period. The server clock is `SMSG_QUERY_TIME_RESPONSE`'s unix time plus the time since; the
//! realm clock is the game clock `SMSG_LOGIN_SETTIMESPEED` seeds, with its packed date.

use std::cell::RefCell;
use std::time::Instant;

use mlua::{Function, IntoLuaMulti, Lua, RegistryKey, Table, Value};

use crate::lua::{is_number, none, to_int, to_number, Api};
use crate::Ca;

/// A ticker's shortest period, so a 0 period cannot spin.
const MIN_PERIOD: f64 = 0.0001;
const SECONDS_PER_DAY: i64 = 86_400;

struct Entry {
    id: u64,
    deadline: f64,
    /// 0 a one-shot.
    period: f64,
    /// -1 forever.
    left: i64,
    cancelled: bool,
    callback: RegistryKey,
}

/// The timer list, the VM's app data so the frame and the natives share it.
struct Timers {
    epoch: Instant,
    next: u64,
    list: Vec<Entry>,
}

impl Timers {
    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }
}

type Shared = RefCell<Timers>;

fn schedule(lua: &Lua, f: Function, seconds: f64, period: f64, left: i64) -> mlua::Result<u64> {
    let key = lua.create_registry_value(f)?;
    let t = lua
        .app_data_ref::<Shared>()
        .ok_or_else(|| mlua::Error::runtime("C_Timer is not installed"))?;
    let mut t = t.borrow_mut();
    let id = t.next;
    t.next += 1;
    let deadline = t.now() + seconds.max(0.0);
    t.list.push(Entry {
        id,
        deadline,
        period,
        left,
        cancelled: false,
        callback: key,
    });
    Ok(id)
}

/// The handle `NewTimer` and `NewTicker` return: `Cancel` and `IsCancelled`.
fn handle(lua: &Lua, id: u64) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set(
        "Cancel",
        lua.create_function(move |lua, _: mlua::MultiValue| {
            if let Some(t) = lua.app_data_ref::<Shared>() {
                if let Some(e) = t.borrow_mut().list.iter_mut().find(|e| e.id == id) {
                    e.cancelled = true;
                }
            }
            Ok(())
        })?,
    )?;
    t.set(
        "IsCancelled",
        lua.create_function(move |lua, _: mlua::MultiValue| {
            Ok(lua
                .app_data_ref::<Shared>()
                .and_then(|t| {
                    t.borrow()
                        .list
                        .iter()
                        .find(|e| e.id == id)
                        .map(|e| e.cancelled)
                })
                .unwrap_or(true))
        })?,
    )?;
    Ok(t)
}

/// The frame: call every due callback in turn, re-arm the tickers, drop the finished. An error
/// goes to the script error handler, as a failed `OnUpdate` does.
pub(crate) fn tick(lua: &Lua) {
    let due: Vec<u64> = {
        let Some(t) = lua.app_data_ref::<Shared>() else {
            return;
        };
        let t = t.borrow();
        let now = t.now();
        t.list
            .iter()
            .filter(|e| !e.cancelled && e.left != 0 && e.deadline <= now)
            .map(|e| e.id)
            .collect()
    };
    for id in due {
        let f = {
            let Some(t) = lua.app_data_ref::<Shared>() else {
                return;
            };
            let t = t.borrow();
            let Some(e) = t.list.iter().find(|e| e.id == id && !e.cancelled) else {
                continue;
            };
            lua.registry_value::<Value>(&e.callback).ok()
        };
        let Some(Value::Function(f)) = f else {
            cancel(lua, id);
            continue;
        };
        if let Err(e) = f.call::<()>(()) {
            report(lua, &e);
        }
        let Some(t) = lua.app_data_ref::<Shared>() else {
            return;
        };
        let mut t = t.borrow_mut();
        if let Some(e) = t.list.iter_mut().find(|e| e.id == id && !e.cancelled) {
            if e.period > 0.0 {
                e.deadline += e.period;
                if e.left > 0 {
                    e.left -= 1;
                }
                if e.left == 0 {
                    e.cancelled = true;
                }
            } else {
                e.cancelled = true;
            }
        }
    }
    let gone: Vec<RegistryKey> = {
        let Some(t) = lua.app_data_ref::<Shared>() else {
            return;
        };
        let mut t = t.borrow_mut();
        let (dead, live): (Vec<Entry>, Vec<Entry>) = std::mem::take(&mut t.list)
            .into_iter()
            .partition(|e| e.cancelled);
        t.list = live;
        dead.into_iter().map(|e| e.callback).collect()
    };
    for key in gone {
        let _ = lua.remove_registry_value(key);
    }
}

fn cancel(lua: &Lua, id: u64) {
    if let Some(t) = lua.app_data_ref::<Shared>() {
        if let Some(e) = t.borrow_mut().list.iter_mut().find(|e| e.id == id) {
            e.cancelled = true;
        }
    }
}

/// `geterrorhandler()(message)`.
fn report(lua: &Lua, e: &mlua::Error) {
    let handler = lua
        .globals()
        .get::<Function>("geterrorhandler")
        .and_then(|g| g.call::<Function>(()));
    if let Ok(h) = handler {
        let _ = h.call::<()>(e.to_string());
    }
}

// ---- Calendar arithmetic (proleptic Gregorian, UTC) ----

/// Days since 1970-01-01 of a civil date (`days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count (`civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `_mkgmtime`: out-of-range months and days carry, as the C runtime normalises them.
pub(crate) fn mkgmtime(year: i64, month: i64, day: i64, hour: i64, minute: i64) -> i64 {
    let y = year + (month - 1).div_euclid(12);
    let m = (month - 1).rem_euclid(12) + 1;
    (days_from_civil(y, m, 1) + day - 1) * SECONDS_PER_DAY + hour * 3600 + minute * 60
}

/// `PushCalendarTime`: `{year, month, monthDay, weekday, hour, minute}`, weekday 1 Sunday.
fn calendar_table(lua: &Lua, epoch: i64) -> mlua::Result<Table> {
    let days = epoch.div_euclid(SECONDS_PER_DAY);
    let secs = epoch.rem_euclid(SECONDS_PER_DAY);
    let (y, m, d) = civil_from_days(days);
    let t = lua.create_table()?;
    t.set("year", y)?;
    t.set("month", m)?;
    t.set("monthDay", d)?;
    t.set("weekday", (days + 4).rem_euclid(7) + 1)?;
    t.set("hour", secs / 3600)?;
    t.set("minute", secs % 3600 / 60)?;
    Ok(t)
}

/// `ReadCalendarTime`: year, month and monthDay required; hour and minute optional.
fn read_calendar(v: &Value) -> Option<i64> {
    let Value::Table(t) = v else {
        return None;
    };
    let num = |k: &str| t.get::<Value>(k).ok().filter(is_number).map(|v| to_int(&v));
    let (y, m, d) = (num("year")?, num("month")?, num("monthDay")?);
    Some(mkgmtime(
        y,
        m,
        d,
        num("hour").unwrap_or(0),
        num("minute").unwrap_or(0),
    ))
}

/// The clocks the frame keeps: the server's unix time and when it came, the game date and clock
/// at the last `SMSG_LOGIN_SETTIMESPEED`, and the minute anchor `EpochFromGameTime` counts seconds
/// from.
#[derive(Default)]
pub(crate) struct Clocks {
    pub server: Option<(u32, Instant)>,
    /// `(year since 2000, month 0-based, day 0-based)`.
    pub date: Option<(i64, i64, i64)>,
    minute_anchor: Option<((i64, i64), Instant)>,
}

impl Clocks {
    /// `EpochFromServerSync`.
    fn server_epoch(&self) -> Option<i64> {
        self.server
            .map(|(t, at)| i64::from(t) + at.elapsed().as_secs() as i64)
    }

    /// `EpochFromGameTime`: the game date and clock, plus the seconds since the minute last
    /// turned, at most 59.
    fn realm_epoch(&mut self, hour: i64, minute: i64) -> Option<i64> {
        let (y, m, d) = self.date?;
        let now = Instant::now();
        let anchor = match self.minute_anchor {
            Some((hm, at)) if hm == (hour, minute) => at,
            _ => {
                self.minute_anchor = Some(((hour, minute), now));
                now
            }
        };
        let secs = now.duration_since(anchor).as_secs().min(59) as i64;
        Some(mkgmtime(2000 + y, m + 1, d + 1, hour, minute) + secs)
    }
}

fn game_clock(lua: &Lua) -> (i64, i64) {
    lua.globals()
        .get::<Function>("GetGameTime")
        .and_then(|f| f.call::<(f64, f64)>(()))
        .map_or((0, 0), |(h, m)| (h as i64, m as i64))
}

fn realm_epoch(lua: &Lua, ca: &Ca) -> Option<i64> {
    let (h, m) = game_clock(lua);
    ca.lock().clocks.realm_epoch(h, m)
}

/// `CurrentEpoch`: the synced server clock, else the realm clock.
fn current_epoch(lua: &Lua, ca: &Ca) -> Option<i64> {
    let synced = ca.lock().clocks.server_epoch();
    synced.or_else(|| realm_epoch(lua, ca)).filter(|e| *e > 0)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    api.lua.set_app_data::<Shared>(RefCell::new(Timers {
        epoch: Instant::now(),
        next: 1,
        list: Vec::new(),
    }));

    api.table("C_Timer", "After", |lua, (s, f): (Value, Value)| {
        let (true, Value::Function(f)) = (is_number(&s), f) else {
            return Err(mlua::Error::runtime(
                "Usage: C_Timer.After(seconds, callback)",
            ));
        };
        schedule(lua, f, to_number(&s), 0.0, 1)?;
        Ok(())
    })?;
    api.table("C_Timer", "NewTimer", |lua, (s, f): (Value, Value)| {
        let (true, Value::Function(f)) = (is_number(&s), f) else {
            return Err(mlua::Error::runtime(
                "Usage: local cbObject = C_Timer.NewTimer(seconds, callback)",
            ));
        };
        let id = schedule(lua, f, to_number(&s), 0.0, 1)?;
        handle(lua, id)
    })?;
    api.table(
        "C_Timer",
        "NewTicker",
        |lua, (s, f, n): (Value, Value, Value)| {
            let (true, Value::Function(f)) = (is_number(&s), f) else {
                return Err(mlua::Error::runtime(
                    "Usage: local cbObject = C_Timer.NewTicker(seconds, callback [, iterations])",
                ));
            };
            let period = to_number(&s).max(MIN_PERIOD);
            let left = if is_number(&n) && to_number(&n) >= 1.0 {
                to_number(&n) as i64
            } else {
                -1
            };
            let id = schedule(lua, f, period, period, left)?;
            handle(lua, id)
        },
    )?;

    for (name, unit) in [
        ("AdjustTimeByDays", SECONDS_PER_DAY),
        ("AdjustTimeByMinutes", 60),
    ] {
        api.table("C_DateAndTime", name, move |lua, (t, n): (Value, Value)| {
            if !is_number(&n) {
                return Ok(none());
            }
            match read_calendar(&t) {
                Some(e) if e >= 0 => {
                    calendar_table(lua, e + to_int(&n) * unit)?.into_lua_multi(lua)
                }
                _ => Ok(none()),
            }
        })?;
    }
    api.table(
        "C_DateAndTime",
        "CompareCalendarTime",
        |lua, (a, b): (Value, Value)| match (read_calendar(&a), read_calendar(&b)) {
            (Some(x), Some(y)) => (x.cmp(&y) as i64).into_lua_multi(lua),
            _ => Ok(none()),
        },
    )?;
    api.table(
        "C_DateAndTime",
        "GetCalendarTimeFromEpoch",
        |lua, v: Value| {
            if !is_number(&v) {
                return Ok(none());
            }
            calendar_table(lua, to_number(&v) as i64)?.into_lua_multi(lua)
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_DateAndTime",
        "GetCurrentCalendarTime",
        move |lua, ()| match realm_epoch(lua, &c).filter(|e| *e > 0) {
            Some(e) => calendar_table(lua, e)?.into_lua_multi(lua),
            None => Ok(none()),
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_DateAndTime",
        "GetSecondsUntilDailyReset",
        move |lua, ()| match current_epoch(lua, &c) {
            Some(e) => (SECONDS_PER_DAY - e % SECONDS_PER_DAY).into_lua_multi(lua),
            None => Ok(none()),
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_DateAndTime",
        "GetServerTimeLocal",
        move |lua, ()| match realm_epoch(lua, &c).filter(|e| *e > 0) {
            Some(e) => e.into_lua_multi(lua),
            None => Ok(none()),
        },
    )?;
    let c = api.ca.clone();
    api.global("GetServerTime", move |lua, ()| {
        match current_epoch(lua, &c) {
            Some(e) => e.into_lua_multi(lua),
            None => Ok(none()),
        }
    })?;

    // `GetTimeCached()`: `GetTime()` as it read at the start of this frame.
    let c = api.ca.clone();
    api.global("GetTimeCached", move |lua, ()| {
        let cached = c.lock().mirror.clock.map(|(_, t)| t);
        match cached {
            Some(t) => Ok(t),
            None => lua.globals().get::<Function>("GetTime")?.call::<f64>(()),
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_calendar_round_trips_and_normalises() {
        let e = mkgmtime(2026, 10, 3, 12, 30);
        assert_eq!(e, 1_791_030_600);
        assert_eq!(civil_from_days(e / SECONDS_PER_DAY), (2026, 10, 3));
        // 31 October + 1 day carries into November; month 13 into the next year.
        assert_eq!(mkgmtime(2026, 10, 32, 0, 0), mkgmtime(2026, 11, 1, 0, 0));
        assert_eq!(mkgmtime(2026, 13, 1, 0, 0), mkgmtime(2027, 1, 1, 0, 0));
        // 1970-01-01 was a Thursday: weekday 5 with Sunday 1.
        let lua = Lua::new();
        let t = calendar_table(&lua, 0).unwrap();
        assert_eq!(t.get::<i64>("weekday").unwrap(), 5);
    }
}
