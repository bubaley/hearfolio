// Turns the native "busy" answer into what the user sees: what is running,
// how far it got, and what can be done about it. Pure, so it is testable.
export type BusyActivity = {
  kind: 'idle' | 'transcribe' | 'postprocess' | 'import' | 'recording' | 'download' | 'storage' | 'archive' | 'restart' | string;
  seconds?: number;
  stopping?: boolean;
  operation?: {id: string; kind: string; recordId?: string | null; progress?: {stage?: string; current?: number; total?: number} | null} | null;
};
export type BusyNotice = {
  message: string;
  /** Where "Open" leads; absent when there is nothing to show. */
  open?: {target: 'record'; recordId: string} | {target: 'recorder' | 'models' | 'settings'};
  /** What "Stop" stops; absent when the work cannot be stopped safely. */
  stop?: {target: 'operation'; id: string} | {target: 'recording'};
};

/** Progress in whole percent, or null while it is not measurable yet. */
export function busyPercent(progress?: {current?: number; total?: number} | null): number | null {
  const current = progress?.current ?? 0, total = progress?.total ?? 0;
  if (!(total > 0) || !Number.isFinite(current)) return null;
  // A running job never claims to be done.
  return Math.max(0, Math.min(99, Math.floor(100 * current / total)));
}

export function describeBusy(activity: BusyActivity, recordName: (id: string) => string | null, language: 'ru' | 'en'): BusyNotice {
  const say = (ru: string, en: string) => language === 'ru' ? ru : en;
  const operation = activity.operation || null;
  const recordId = operation?.recordId || null;
  const name = recordId ? recordName(recordId) : null;
  const quoted = name ? say(` «${name}»`, ` “${name}”`) : '';
  const percent = busyPercent(operation?.progress);
  const suffix = percent === null ? '' : ` — ${percent}%`;
  const open = recordId ? {target: 'record' as const, recordId} : undefined;
  const stop = operation?.id && !activity.stopping ? {target: 'operation' as const, id: operation.id} : undefined;
  const waitOrStop = say('Дождитесь окончания или остановите её.', 'Wait for it to finish or stop it.');
  if (activity.stopping && operation) {
    return {message: say(`Останавливаем${quoted ? ` обработку${quoted}` : ' текущую задачу'}. Через несколько секунд можно будет продолжить.`, `Stopping${quoted ? ` work on${quoted}` : ' the current task'}. You can continue in a few seconds.`), open};
  }
  switch (activity.kind) {
    case 'idle':
      return {message: say('Предыдущая задача только что завершилась. Повторите действие.', 'The previous task has just finished. Try again.')};
    case 'transcribe':
      return {message: `${say(`Идёт расшифровка${quoted}`, `Transcribing${quoted}`)}${suffix}. ${waitOrStop}`, open, stop};
    case 'postprocess':
      return {message: `${say(`Идёт обработка текста${quoted}`, `Processing the text of${quoted || ' a recording'}`)}. ${waitOrStop}`, open, stop};
    case 'import':
      return {message: `${say('Запись сохраняется в архив', 'A recording is being saved to the archive')}${suffix}. ${say('Это займёт немного времени.', 'This takes a moment.')}`, open, stop};
    case 'recording':
      return {message: say('Идёт запись на диктофон. Завершите её, чтобы продолжить.', 'The recorder is recording. Finish it to continue.'), open: {target: 'recorder'}, stop: {target: 'recording'}};
    case 'download':
      return {message: say('Скачивается модель распознавания. Дождитесь окончания.', 'A transcription model is downloading. Wait for it to finish.'), open: {target: 'models'}};
    case 'storage':
      return {message: say('Архив переносится в новую папку. Дождитесь окончания.', 'The archive is moving to a new folder. Wait for it to finish.'), open: {target: 'settings'}};
    default:
      return {message: say('Приложение сохраняет изменения. Повторите через несколько секунд.', 'The app is saving changes. Try again in a few seconds.')};
  }
}
