import {test} from 'node:test';
import assert from 'node:assert/strict';
import {initialSettingsDraft, hasSettingsChanges, mergeSettingsDraft} from './settings-draft.ts';

const settings={openrouterUrl:'https://openrouter.ai/api/v1',language:'system',hasToken:true};
test('language section retains connection values and does not clear a stored token',()=>{
  const draft=initialSettingsDraft(settings),values=new FormData();values.set('language','en');
  const changed=mergeSettingsDraft(values,draft);
  assert.equal(changed.url,settings.openrouterUrl);assert.equal(changed.token,'');assert.equal(changed.language,'en');assert.equal(hasSettingsChanges(changed,settings),true);
});
test('connection section retains language and preserves the raw token draft',()=>{
  const values=new FormData();values.set('url','https://example.org/v1');values.set('token',' test-token ');
  const changed=mergeSettingsDraft(values,{...initialSettingsDraft(settings),language:'en'});
  assert.equal(changed.language,'en');assert.equal(changed.token,' test-token ');assert.equal(hasSettingsChanges(changed,settings),true);
});
test('reverting all fields clears dirty state, including trimmed empty token',()=>{
  assert.equal(hasSettingsChanges({...initialSettingsDraft(settings),url:` ${settings.openrouterUrl} `,token:'  '},settings),false);
  assert.equal(hasSettingsChanges({...initialSettingsDraft(settings),token:'new-token'},settings),true);
});
