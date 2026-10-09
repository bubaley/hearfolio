package com.bubaley.hearfolio.recording

import androidx.core.content.FileProvider

// Manifest providers are merged by class name. Keep our narrowly scoped cache
// provider separate from the FileProvider installed by the generated Tauri app.
class HearfolioFileProvider : FileProvider()
