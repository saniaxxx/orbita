# qsg-asr EOU patch (`feat/asr-eou`)

Patch for the stand clone `github.com/v3rt3p/quasistreaming-gigam` at
`e1770f1` (`fix: make it runnable on macos`). It is not an upstream change.

## Why

Ten VAD on the gained station stream marks amplified room noise as speech.
The end-of-utterance silence never reaches 800 ms, so quasar receives its first
`end_of_utt=true` only at `max_utterance` (8 s) or later. Example: `Ты кто?`
partial at 11:32:01.7, final at 11:32:06.5.

The same patch also removes a second gain on the recognition path: the
streaming path already multiplies by `INPUT_GAIN`; `recognize_audio` multiplied
by it again, feeding GigaAM values far outside `[-1, 1]`. On the stand
recordings that double gain garbled the text (`Кто?` instead of
`Алиса, ты кто?`).

## Changes

- `asr_core/processors/vad_processor.py`
  - `VAD_ENERGY_GATE_RATIO` (0 = off): drop VAD segments whose int16 RMS is
    below `peak_segment_rms * ratio`; the peak is updated only from kept
    segments and reset per utterance.
  - `VAD_ENERGY_GATE_MAX_RMS` (default 900): caps the threshold so a loud
    transient cannot suppress quieter speech. It is raw int16 RMS **after**
    `INPUT_GAIN`; 900 is tuned for the station's `INPUT_GAIN=30`.
  - `VAD_ENERGY_GATE_DEBUG=1` logs `vad_energy_gate` decisions.
- `quasistreaming-server.py`
  - `INPUT_GAIN` is applied once, in the streaming path.
  - `RECOGNIZE_GAIN` (default 1) is used by `recognize_audio`; do not set it
    to `INPUT_GAIN` on this path.
  - Startup log prints both gains.

## Stand configuration

`/etc/systemd/system/qsg-asr.service.d/10-orbita-eou.conf`:

```ini
[Service]
Environment=RECOGNIZE_GAIN=1
Environment=VAD_ENERGY_GATE_RATIO=0.3
Environment=VAD_ENERGY_GATE_MAX_RMS=900
```

`INPUT_GAIN=30` and the rest stay in `qsg-asr.service`.

Apply:

```sh
cd /opt/qsg
git apply /path/to/deploy/asr/qsg-orbita.patch
systemctl daemon-reload
systemctl restart qsg-asr
journalctl -u qsg-asr -n 20 --no-pager   # started on :8006 (INPUT_GAIN=30.0 RECOGNIZE_GAIN=1.0)
```

Rollback:

```sh
cd /opt/qsg
git checkout -- asr_core/processors/vad_processor.py quasistreaming-server.py
rm -f /etc/systemd/system/qsg-asr.service.d/10-orbita-eou.conf
systemctl daemon-reload
systemctl restart qsg-asr
```

Backup on CT100: `/root/backup-qsg-eou-20261108/`.

## Verification

- **L2, CPU test instance, stand recordings:** `Алиса, ты кто?` final in
  2.98 s (full text); long 9 s phrase EOU detected about 1 s after the last
  speech instead of `max_utterance`; `Алиса, … pause … вопрос` keeps the full
  question; no premature `С.` final at `VAD_MIN_SPEECH_DURATION=1.5`.
- **L3, live `qsg-asr` on GPU, direct replay to `:8006`:** `Алиса, ты кто?`
  final 2.59 s; long phrase final 7.17 s with full text; pause phrase final
  3.84 s with full text; every final `reason=eou_silence`. `llama` stayed
  active, no new restarts.
- **Voice through the simulator (Vosk TTS → Ogg → quasar):** ASR returned
  `end_of_utt=true` and the full text. The response path (`@@mm_semantic_frame`
  only, no adapter/LLM call) timed out during the streaming work; that is not
  an ASR result.

Voice recordings under `/tmp/asr-audio` are private and are not copied into
this repository. Test harness and calibration scripts live only on CT100.
