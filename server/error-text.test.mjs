import {test} from 'node:test';
import assert from 'node:assert/strict';

// i18n reads the browser language at import time.
globalThis.navigator??={language:'ru-RU',languages:['ru-RU']};
globalThis.document??={documentElement:{},title:''};
const {errorText,setLanguage}=await import('../src/i18n.ts');

test('a service status becomes advice; its raw message never reaches the screen',()=>{
  setLanguage('ru');
  assert.match(errorText('errors.cloudHttp|HTTP 401: No auth credentials found sk-or-secret'),/API token/);
  assert.match(errorText('errors.cloudHttp|HTTP 402: Insufficient credits'),/баланс/i);
  assert.match(errorText('errors.cloudHttp|HTTP 429: Rate limit exceeded'),/Подождите/);
  assert.match(errorText('errors.cloudHttp|HTTP 503: upstream connect error'),/временно недоступен/);
  assert.match(errorText('errors.cloudHttp|HTTP 404: No endpoints found'),/модель/i);
  assert.match(errorText('errors.cloudHttp|HTTP 418: teapot'),/Сервис отклонил запрос/);
  for(const raw of ['errors.cloudHttp|HTTP 401: sk-or-secret','errors.cloudService|Provider returned error {"code":502}','errors.operationFailed|JoinError::Panic(...)'])assert.doesNotMatch(errorText(raw),/sk-or|Provider|JoinError|HTTP|\{/);
  setLanguage('en');
  assert.match(errorText('errors.cloudHttp|HTTP 402: Insufficient credits'),/balance/);
  assert.match(errorText(new Error('errors.cloudHttp|HTTP 500: boom')),/temporarily unavailable/);
});

test('system text is replaced by a plain message, app text is kept and translated',()=>{
  setLanguage('ru');
  const generic=errorText('errors.operationFailed');
  assert.equal(errorText('No such file or directory (os error 2)'),generic);
  assert.equal(errorText(new TypeError("Cannot read properties of undefined (reading 'id')")),generic);
  assert.equal(errorText('errors.somethingNew'),generic);
  assert.equal(errorText('Укажите имя этого устройства.'),'Укажите имя этого устройства.');
  assert.equal(errorText('progress.prepareAudio'),'Подготавливаем аудио');
  setLanguage('en');
  assert.doesNotMatch(errorText('Не удалось прочитать файл /data/x'),/[А-Яа-я]/);
});
