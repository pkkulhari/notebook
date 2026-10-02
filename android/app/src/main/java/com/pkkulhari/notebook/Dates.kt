package com.pkkulhari.notebook

import java.time.Duration
import java.time.Instant
import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter
import java.time.temporal.ChronoUnit

private val WEEKDAY = DateTimeFormatter.ofPattern("EEE")
private val MONTH_DAY = DateTimeFormatter.ofPattern("MMM d")
private val FULL_DATE = DateTimeFormatter.ofPattern("MMM d, yyyy")

/**
 * A note's date in the list, as the desktop shows it: "Now" within a minute,
 * then "Today", "Yesterday", a weekday within the week, and the date.
 */
fun noteDate(millis: Long, now: ZonedDateTime = ZonedDateTime.now()): String {
    val date = Instant.ofEpochMilli(millis).atZone(now.zone)
    if (Duration.between(date, now).seconds in 0 until 60) return "Now"
    val days = ChronoUnit.DAYS.between(date.toLocalDate(), now.toLocalDate())
    return when {
        days == 0L -> "Today"
        days == 1L -> "Yesterday"
        days in 2..6 -> date.format(WEEKDAY)
        date.year == now.year -> date.format(MONTH_DAY)
        else -> date.format(FULL_DATE)
    }
}
