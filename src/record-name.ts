const audioExtension = /\.(m4a|mp3|wav|aac|flac|ogg|webm|mp4|opus|aiff|aif|caf|wma)$/i;

// The display title is editable. The archive's audio path and format are not.
export function recordNameParts(name: string, inputPath: string): {title: string; extension: string} {
  const named = name.match(audioExtension);
  const extension = named?.[0] || inputPath.match(audioExtension)?.[0] || '';
  return {title: named ? name.slice(0, -named[0].length) : name, extension};
}

export function renamedRecordTitle(title: string, extension: string): string {
  let value = title.trim();
  if (extension && value.toLowerCase().endsWith(extension.toLowerCase())) value = value.slice(0, -extension.length).trim();
  return value ? value + extension : '';
}
