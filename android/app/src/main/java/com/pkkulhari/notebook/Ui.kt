package com.pkkulhari.notebook

import android.content.Context
import android.text.TextUtils
import android.util.TypedValue

fun Context.dp(value: Int) = TypedValue.applyDimension(
    TypedValue.COMPLEX_UNIT_DIP, value.toFloat(), resources.displayMetrics,
).toInt()

/** Where the line holding `at` starts. */
fun lineStart(text: CharSequence, at: Int) = if (at == 0) 0 else TextUtils.lastIndexOf(text, '\n', at - 1) + 1
