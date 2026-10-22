import type {LanguagePreference} from './i18n';
import type {Settings} from './api';

export const DEFAULT_CHUNK_SECONDS=120;
export const CHUNK_CHOICES=[30,60,120,180,300] as const;
export type SettingsDraft = {url:string;token:string;language:LanguagePreference;chunkSeconds:number};
export function initialSettingsDraft(settings:Settings):SettingsDraft {return {url:settings.openrouterUrl,token:'',language:settings.language,chunkSeconds:settings.chunkSeconds||DEFAULT_CHUNK_SECONDS};}
export function hasSettingsChanges(draft:SettingsDraft, settings:Settings):boolean {return draft.url.trim()!==settings.openrouterUrl||Boolean(draft.token.trim())||draft.language!==settings.language||draft.chunkSeconds!==(settings.chunkSeconds||DEFAULT_CHUNK_SECONDS);}
/** A section form contains only its own fields; absent fields retain their draft. */
export function mergeSettingsDraft(values:FormData, draft:SettingsDraft):SettingsDraft {
  return {url:values.has('url')?String(values.get('url')):draft.url,token:values.has('token')?String(values.get('token')):draft.token,language:values.has('language')?String(values.get('language')) as LanguagePreference:draft.language,chunkSeconds:values.has('chunkSeconds')?Number(values.get('chunkSeconds'))||draft.chunkSeconds:draft.chunkSeconds};
}
