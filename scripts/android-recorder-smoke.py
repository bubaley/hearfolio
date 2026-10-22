#!/usr/bin/env python3
"""Exercise the installed Android APK through its real debug WebView buttons.

Requires adb and Node >=22 (native WebSocket). No emulator microphone input or
network service is needed: silent PCM still proves that capture actually runs.
"""
import argparse
import io
import json
import pathlib
import re
import subprocess
import time
import urllib.request
import wave

CDP = r"""
const [url, expression] = process.argv.slice(1);
const socket = new WebSocket(url);
let completed = false;
const timeout = setTimeout(() => { console.error('CDP response timeout'); process.exit(1); }, 20000);
socket.addEventListener('error', () => { if (!completed) { console.error('CDP connection failed'); process.exit(1); } });
socket.addEventListener('open', () => socket.send(JSON.stringify({id:1,method:'Runtime.evaluate',params:{expression,awaitPromise:true,returnByValue:true}})));
socket.addEventListener('message', ({data}) => {
  const message = JSON.parse(data);
  if (message.id !== 1) return;
  clearTimeout(timeout);
  completed = true;
  if (message.error || message.result.exceptionDetails) {console.error(JSON.stringify(message));process.exit(1);}
  console.log(JSON.stringify(message.result.result.value ?? null)); socket.close();
});
"""

class Smoke:
    def __init__(self, args):
        self.args = args
        self.output = pathlib.Path(args.output)
        self.output.mkdir(parents=True, exist_ok=True)
        self.socket = None
        self.forward = None
        self.snapshots = []
        self.pcm_readiness = []
        self.events = []

    def adb(self, *command, binary=False, check=True, timeout=30):
        serial = ['-s', self.args.serial] if self.args.serial else []
        result = subprocess.run(['adb', *serial, *command], capture_output=True, timeout=timeout)
        if check and result.returncode:
            raise RuntimeError(f'adb {command}: {result.stderr.decode(errors="replace")}')
        return result.stdout if binary else result.stdout.decode(errors='replace').strip()

    def foreground(self):
        activities = self.adb('shell', 'dumpsys', 'activity', 'activities')
        top = '\n'.join(line for line in activities.splitlines() if 'topResumedActivity' in line)
        if self.args.package not in top:
            raise RuntimeError(f'Application lost foreground: {top}')
        crashes = self.adb('logcat', '-d', '-b', 'crash')
        if self.args.package in crashes and ('FATAL EXCEPTION' in crashes or 'Fatal signal' in crashes):
            raise RuntimeError('Application crash detected; see logcat-crash.txt')

    def connect(self):
        pid = self.adb('shell', 'pidof', self.args.package).split()[0]
        sockets = self.adb('shell', 'cat', '/proc/net/unix')
        candidates = re.findall(r'@([^\s]*webview_devtools_remote[^\s]*)', sockets)
        candidates.sort(key=lambda name: pid not in name)
        for name in candidates:
            port = self.adb('forward', 'tcp:0', f'localabstract:{name}')
            try:
                with urllib.request.urlopen(f'http://127.0.0.1:{port}/json', timeout=3) as response:
                    targets = json.load(response)
                for target in targets:
                    if target.get('type') == 'page' and target.get('webSocketDebuggerUrl'):
                        self.socket = target['webSocketDebuggerUrl']
                        self.forward = port
                        return True
            except (OSError, ValueError):
                pass
            self.adb('forward', '--remove', f'tcp:{port}', check=False)
        return False

    def evaluate(self, expression):
        result = subprocess.run(['node', '--input-type=module', '-e', CDP, self.socket, expression], capture_output=True, text=True, timeout=25)
        if result.returncode:
            raise RuntimeError(f'CDP: {result.stderr}')
        return json.loads(result.stdout)

    def click(self, selector):
        self.wait(lambda: self.evaluate(f'!!document.querySelector({json.dumps(selector)}) && !document.querySelector({json.dumps(selector)}).disabled'), f'enabled button {selector}')
        self.evaluate(f'document.querySelector({json.dumps(selector)}).click()')

    def wait(self, predicate, label, timeout=35, check_foreground=True):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            try:
                if check_foreground:
                    self.foreground()
                value = predicate()
                if value:
                    return value
            except (RuntimeError, OSError, IndexError) as error:
                last = str(error)
                # A crash is not a transient CDP or loading error.
                if 'lost foreground' in last or 'crash detected' in last:
                    raise
            time.sleep(0.3)
        raise RuntimeError(f'Timed out waiting for {label}: {last}')

    def invoke(self, name):
        return self.evaluate(f'window.__TAURI_INTERNALS__.invoke({json.dumps(name)})')

    def status(self):
        status = self.invoke('recording_activity_snapshot')
        self.snapshots.append(status)
        return status

    def state(self, state):
        status = self.status()
        return status if status.get('state') == state else None

    def history_entry(self, identifier):
        return next((entry for entry in self.invoke('list_history') if entry['id'] == identifier), None)

    def native_journal(self, source):
        return json.loads(self.adb('exec-out', 'run-as', self.args.package, 'cat', source + '.state.json'))

    def event(self, action, status=None):
        self.events.append({'action': action, 'monotonicSeconds': time.monotonic(), 'status': status})

    def reconnect(self):
        old_forward = self.forward
        self.wait(self.connect, 'debug WebView after foreground return', timeout=30)
        if old_forward and old_forward != self.forward:
            self.adb('forward', '--remove', f'tcp:{old_forward}', check=False)

    def background_capture(self, source):
        for mode in ['screen-off', 'home']:
            baseline = self.native_journal(source)
            self.event(mode + '-begin', baseline)
            if mode == 'screen-off':
                self.adb('shell', 'input', 'keyevent', '223')  # KEYCODE_SLEEP
            else:
                self.adb('shell', 'input', 'keyevent', '3')  # KEYCODE_HOME
            time.sleep(2)
            def advanced():
                status = self.native_journal(source)
                assert status['path'] == source and status['state'] == 'recording', status
                return status if status.get('elapsedMillis', 0) > baseline.get('elapsedMillis', 0) else None
            # Foreground is intentionally absent here; read the service journal
            # directly, without calling the WebView or any Tauri command.
            status = self.wait(advanced, mode + ' native capture progress', timeout=12, check_foreground=False)
            self.event(mode + '-progress', status)
            if mode == 'screen-off':
                self.adb('shell', 'input', 'keyevent', '224')  # KEYCODE_WAKEUP
                self.adb('shell', 'input', 'keyevent', '82')  # unlock emulator
            else:
                self.adb('shell', 'am', 'start', '-W', '-f', '0x24000000', '-n', self.args.package + '/.MainActivity')
            def foreground_restored():
                try:
                    self.foreground()
                    return True
                except RuntimeError as error:
                    if 'crash detected' in str(error):
                        raise
                    return False
            self.wait(foreground_restored, 'foreground restored', timeout=15, check_foreground=False)
            self.reconnect()
            restored = self.wait(lambda: self.state('recording'), 'same recording restored')
            assert restored['path'] == source, 'Background return replaced the recording session'
            self.event(mode + '-foreground-restored', restored)

    def recording(self, pause=False):
        # The home screen's record button starts a capture from the record list.
        self.click('#quick-record')
        status = self.wait(lambda: self.state('recording'), 'native recording state')
        source = status['path']
        self.wait(lambda: self.state('recording') and self.status().get('elapsedMillis', 0) >= 1500, 'measured PCM duration')
        def measured_journal():
            journal = self.adb('exec-out', 'run-as', self.args.package, 'cat', source + '.state.json')
            native = json.loads(journal)
            return native if native.get('elapsedMillis', 0) > 0 else None
        native = self.wait(measured_journal, 'measured native journal')
        assert native['path'] == source and native['state'] == 'recording', native
        assert native.get('elapsedMillis', 0) > 0, native
        def flushed_pcm():
            audio = self.adb('exec-out', 'run-as', self.args.package, 'cat', source, binary=True)
            observation = {'path': source, 'bytes': len(audio), 'headerHex': audio[:44].hex(), 'elapsedMillis': native.get('elapsedMillis')}
            self.pcm_readiness.append(observation)
            # Native capture time counts accepted samples. Buffered WAV output
            # can still contain only its header until a later disk flush.
            return observation if len(audio) > 44 and audio[:4] == b'RIFF' and audio[8:12] == b'WAVE' else None
        pcm = self.wait(flushed_pcm, 'flushed RIFF/WAVE PCM bytes', timeout=35)
        print(f'Captured PCM readiness: {json.dumps(pcm)}')
        if pause:
            self.background_capture(source)
        if pause:
            self.click('#pause-recording')
            self.wait(lambda: self.state('paused'), 'native pause')
            # A PCM read already accepted before pause may finish writing once.
            time.sleep(0.25)
            baseline = self.state('paused')['elapsedMillis']
            time.sleep(1.2)
            paused = self.state('paused')
            assert paused and paused['elapsedMillis'] == baseline, 'Capture clock advanced during pause'
            self.click('#pause-recording')
            self.wait(lambda: self.state('recording') and self.status()['elapsedMillis'] > baseline + 500, 'native resume')
        self.click('#stop-recording')
        identifier = 'hearing-recording-' + pathlib.Path(source).stem.removeprefix('hearfolio-recording-')
        entry = self.wait(lambda: self.history_entry(identifier), 'durable archived recording', timeout=60)
        # The archive write precedes the UI's import completion. Do not start
        # another capture while that foreground operation is still settling.
        self.wait(lambda: self.evaluate(f'!!document.querySelector("[data-rename=\\"{identifier}\\"]") && !!document.querySelector("[data-back]:not(:disabled):not([hidden])")'), 'saved recording UI')
        audio = self.adb('exec-out', 'run-as', self.args.package, 'cat', entry['inputPath'], binary=True)
        # Finished recordings are compacted to AAC in MP4; the WAV is kept only when compaction fails.
        if entry['inputPath'].endswith('.m4a'):
            assert len(audio) > 1024 and audio[4:8] == b'ftyp', 'Invalid compacted AAC recording'
            extension = 'm4a'
        else:
            with wave.open(io.BytesIO(audio)) as wav:
                assert wav.getnchannels() == 1 and wav.getsampwidth() == 2 and wav.getnframes() > 0, 'Invalid recorded PCM WAV'
            extension = 'wav'
        self.event('archived-audio', {'path': entry['inputPath'], 'bytes': len(audio)})
        (self.output / f'{identifier}.{extension}').write_bytes(audio)
        self.foreground()
        return identifier

    def run(self):
        # A newly booted hosted emulator can spend longer optimizing the APK.
        # Keep the UI/capture deadlines unchanged; allow extra install time only.
        self.adb('install', '-r', self.args.apk, timeout=120)
        self.adb('shell', 'pm', 'clear', self.args.package)
        for permission in ['android.permission.RECORD_AUDIO', 'android.permission.POST_NOTIFICATIONS']:
            self.adb('shell', 'pm', 'grant', self.args.package, permission)
        self.adb('logcat', '-c')
        self.adb('shell', 'am', 'start', '-W', '-n', self.args.package + '/.MainActivity')
        self.wait(self.connect, 'debug WebView CDP', timeout=60)
        self.wait(lambda: self.evaluate('!!window.__TAURI_INTERNALS__'), 'Tauri bridge')
        first = self.recording(pause=True)
        self.click('[data-back]')
        second = self.recording()
        assert first != second, 'Second capture reused the previous session'
        print('PASS: real UI start/screen-off/home/pause/resume/stop/restart; native journal, PCM WAV, foreground and crash checks')

    def diagnostics(self):
        for buffer in ['main', 'crash']:
            (self.output / f'logcat-{buffer}.txt').write_text(self.adb('logcat', '-d', '-b', buffer, check=False))
        (self.output / 'activity.txt').write_text(self.adb('shell', 'dumpsys', 'activity', 'activities', check=False))
        (self.output / 'snapshots.json').write_text(json.dumps(self.snapshots, indent=2))
        (self.output / 'pcm-readiness.json').write_text(json.dumps(self.pcm_readiness, indent=2))
        (self.output / 'events.json').write_text(json.dumps(self.events, indent=2))
        (self.output / 'screen.png').write_bytes(self.adb('exec-out', 'screencap', '-p', binary=True, check=False))
        if self.forward:
            self.adb('forward', '--remove', f'tcp:{self.forward}', check=False)

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apk', required=True)
    parser.add_argument('--serial')
    parser.add_argument('--package', default='com.bubaley.speechdesk')
    parser.add_argument('--output', default='android-runtime-results')
    args = parser.parse_args()
    if not pathlib.Path(args.apk).is_file():
        parser.error(f'APK must be an existing file: {args.apk!r}')
    smoke = Smoke(args)
    try:
        smoke.run()
    finally:
        smoke.diagnostics()
