package com.bubaley.hearfolio.recording

import android.content.Context
import android.util.AtomicFile
import java.io.File
import org.json.JSONObject

internal object NativeJobJournal {
    fun privateFile(context: Context,path: String?): File {
        val file=File(requireNotNull(path)).canonicalFile
        require(file.path.startsWith(context.dataDir.canonicalPath+File.separator) && file.name.endsWith(".json"))
        return file
    }
    fun read(file: File?): JSONObject? = try { file?.let { JSONObject(it.readText()) } } catch(_:Exception) { null }
    fun write(file: File?,value: JSONObject) {
        if(file == null) return
        val atomic=AtomicFile(file);val stream=atomic.startWrite()
        try {stream.write(value.toString().toByteArray());atomic.finishWrite(stream)} catch(error:Exception) {atomic.failWrite(stream);throw error}
    }
}
