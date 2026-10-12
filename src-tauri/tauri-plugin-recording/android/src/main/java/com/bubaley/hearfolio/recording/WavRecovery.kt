package com.bubaley.hearfolio.recording

import java.io.File
import java.io.RandomAccessFile

// Checkpoints retain a recognizable PCM header. A process killed between
// checkpoints can be recovered from actual length, without replaying capture.
internal object WavRecovery {
    fun header(wav: RandomAccessFile, count: Long, rate: Int) {
        require(count in 0..(0xffffffffL - 36) && count % 2 == 0L)
        val position = wav.filePointer
        wav.seek(0)
        fun little(value: Long, size: Int) { repeat(size) { wav.write((value shr (it * 8)).toInt() and 255) } }
        wav.writeBytes("RIFF"); little(count + 36,4); wav.writeBytes("WAVEfmt ")
        little(16,4); little(1,2); little(1,2); little(rate.toLong(),4)
        little(rate * 2L,4); little(2,2); little(16,2); wav.writeBytes("data"); little(count,4)
        wav.seek(maxOf(position,44))
    }
    fun recover(file: File): Long? = try {
        RandomAccessFile(file,"rw").use { wav ->
            if(wav.length() <= 44) return null
            val bytes = ByteArray(44); wav.readFully(bytes)
            fun tag(offset: Int, value: String) = String(bytes,offset,value.length,Charsets.US_ASCII) == value
            fun little(offset: Int,size: Int): Long { var value=0L;repeat(size){value=value or ((bytes[offset+it].toLong() and 255) shl (8*it))};return value }
            if(!tag(0,"RIFF") || !tag(8,"WAVEfmt ") || !tag(36,"data") || little(20,2)!=1L || little(22,2)!=1L || little(34,2)!=16L) return null
            val rate=little(24,4).toInt(); if(rate !in 8000..192000) return null
            val count=(wav.length()-44)/2*2
            if(count > 0xffffffffL-36) return null
            wav.setLength(count+44); header(wav,count,rate);wav.fd.sync()
            count / (rate*2L)
        }
    } catch(_: Exception) { null }
}
