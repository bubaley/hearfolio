import type {LanguagePreference} from './i18n';
import type {Settings} from './api';

export type SettingsDraft = {url:string;token:string;language:LanguagePreference};
export function initialSettingsDraft(settings:Settings):SettingsDraft {return {url:settings.openrouterUrl,token:'',language:settings.language};}
export function hasSettingsChanges(draft:SettingsDraft, settings:Settings):boolean {return draft.url.trim()!==settings.openrouterUrl||Boolean(draft.token.trim())||draft.language!==settings.language;}
/** A section form contains only its own fields; absent fields retain their draft. */
export function mergeSettingsDraft(values:FormData, draft:SettingsDraft):SettingsDraft {
  return {url:values.has('url')?String(values.get('url')):draft.url,token:values.has('token')?String(values.get('token')):draft.token,language:values.has('language')?String(values.get('language')) as LanguagePreference:draft.language};
}
