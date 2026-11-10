// Shared date/time helpers used by several built-in templates.

// Seconds since 1970-01-01 as `YYYY-MM-DD hh:mm:ss UTC` (proleptic
// Gregorian calendar, Howard Hinnant's civil-from-days algorithm).
fn unix_time_text(s64 t) {
    if (t == 0) return "0 (not set)";
    s64 days = t / 86400;
    s64 secs = t % 86400;
    if (secs < 0) {
        secs += 86400;
        days -= 1;
    }
    s64 z = days + 719468;
    s64 era = (z >= 0 ? z : z - 146096) / 146097;
    s64 doe = z - era * 146097;
    s64 yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    s64 year = yoe + era * 400;
    s64 doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    s64 mp = (5 * doy + 2) / 153;
    s64 day = doy - (153 * mp + 2) / 5 + 1;
    s64 month = mp < 10 ? mp + 3 : mp - 9;
    if (month <= 2) year += 1;
    return std::format("{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        year, month, day, secs / 3600, secs / 60 % 60, secs % 60);
};

// MS-DOS packed date (FAT, ZIP).
fn dos_date_text(u16 v) {
    return std::format("{:04}-{:02}-{:02}", (v >> 9) + 1980, (v >> 5) & 0x0F, v & 0x1F);
};

// MS-DOS packed time with two-second resolution (FAT, ZIP).
fn dos_time_text(u16 v) {
    return std::format("{:02}:{:02}:{:02}", v >> 11, (v >> 5) & 0x3F, (v & 0x1F) * 2);
};

