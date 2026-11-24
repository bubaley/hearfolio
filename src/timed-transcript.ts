// The timed transcript list, shared by device records (main.ts) and cloud recordings (org.ts).
import {escapeHtml as esc} from './icons.ts';
import {formatClock, speakerHeading, type Segment} from './transcript.ts';

/** `list` is already joined into paragraphs; `names` comes from `speakerNames(list, …)`. */
export function timedListHtml(list:readonly Segment[],names:Map<string,string>,options:{id:string;lang:string;label:string;seekLabel:string}):string {
  const order=[...names.keys()];
  return `<ol class="timed-transcript" id="${esc(options.id)}" lang="${esc(options.lang)}" aria-label="${esc(options.label)}">${list.map((segment,index)=>{
    const heading=speakerHeading(list,names,index),tone=segment.speaker?order.indexOf(segment.speaker):-1,clock=formatClock(segment.start_ms);
    return `<li class="timed-row" data-row="${index}"><button type="button" class="timed-time" data-seek-ms="${segment.start_ms}" aria-label="${esc(options.seekLabel+clock)}">${clock}</button><div class="timed-body">${heading?`<strong class="timed-speaker tone-${tone<0?'none':tone%6}">${esc(heading)}</strong>`:''}<p>${esc(segment.text)}</p></div></li>`;
  }).join('')}</ol>`;
}
