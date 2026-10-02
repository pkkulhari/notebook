package com.pkkulhari.notebook

import android.content.Context
import android.icu.text.DateFormat
import android.icu.util.TimeZone
import java.time.Duration
import java.time.Instant
import java.time.ZonedDateTime
import java.time.temporal.ChronoUnit
import java.util.Date
import java.util.Locale

/** A note's or a sync's date, by the desktop's rules, in the user's language. */
fun noteDate(context: Context, millis: Long, now: ZonedDateTime = ZonedDateTime.now()): String {
    val date = Instant.ofEpochMilli(millis).atZone(now.zone)
    if (Duration.between(date, now).seconds in 0 until 60) return context.getString(R.string.now)
    val days = ChronoUnit.DAYS.between(date.toLocalDate(), now.toLocalDate())
    val skeleton = when {
        days == 0L -> return context.getString(R.string.today)
        days == 1L -> return context.getString(R.string.yesterday)
        days in 2..6 -> DateFormat.ABBR_WEEKDAY
        date.year == now.year -> DateFormat.ABBR_MONTH_DAY
        else -> DateFormat.YEAR_ABBR_MONTH_DAY
    }
    // The skeleton names the fields; the locale orders and spells them.
    return DateFormat.getInstanceForSkeleton(skeleton, Locale.getDefault())
        .apply { timeZone = TimeZone.getTimeZone(now.zone.id) }
        .format(Date(millis))
}
