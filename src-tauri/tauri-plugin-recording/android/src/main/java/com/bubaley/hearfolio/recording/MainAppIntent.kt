package com.bubaley.hearfolio.recording

import android.content.Context
import android.content.Intent

// There are now two launcher entries. Notification taps must open the archive
// UI, never choose the quick recorder through an unspecified package launch.
fun mainAppIntent(context: Context): Intent = Intent(Intent.ACTION_MAIN)
    .addCategory(Intent.CATEGORY_LAUNCHER)
    .setClassName(context.packageName, context.packageName + ".MainActivity")
    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
