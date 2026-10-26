import asyncio

import aiohttp
import pytest

from orbita.config import OpenAIConfig
from orbita.http_store import AudioStore, AudioStream
from orbita.openai_io import OpenAIClient
from orbita.sentences import PhraseSplitter
from orbita.streaming import StreamingAnswer
from orbita.wyoming_io import Pcm, from_wav
from tests.fakes import FakeOpenAI

# ---------------- phrases ----------------


def split(deltas, min_chars=20):
    sp = PhraseSplitter(min_chars)
    out = []
    for d in deltas:
        out += sp.feed(d)
    return out, sp.flush()


def test_phrases_from_deltas():
    text = "Сегодня в Москве плюс пять градусов. Вечером возможен дождь, возьмите зонт! Удачи."
    out, rest = split(list(text))  # one character at a time
    assert out == ["Сегодня в Москве плюс пять градусов.", "Вечером возможен дождь, возьмите зонт!"]
    assert rest == ["Удачи."]


def test_short_phrases_join_the_next():
    out, rest = split(["Да. ", "Конечно, это можно сделать сегодня. ", "Ок"])
    assert out == ["Да. Конечно, это можно сделать сегодня."]
    assert rest == ["Ок"]


def test_numbers_and_abbreviations_do_not_split():
    out, rest = split(["Курс 3.5 процента, т.е. ", "немного выше прошлого месяца. Всё."])
    assert out == ["Курс 3.5 процента, т.е. немного выше прошлого месяца."]
    assert rest == ["Всё."]


def test_line_breaks_and_long_first_phrase():
    out, _ = split(["Первый пункт списка покупок\nВторой пункт списка покупок\n"])
    assert out == ["Первый пункт списка покупок", "Второй пункт списка покупок"]
    long = "слово, " * 40
    out, _ = split([long])
    assert out and out[0].endswith(",") and len(out[0]) <= 161


# ---------------- the WAV stream ----------------


async def test_audio_stream_readers_and_formats():
    st = AudioStream()
    got = []

    async def read():
        data = b""
        async for c in st.chunks():
            data += c
        got.append(data)

    early = asyncio.create_task(read())
    await asyncio.sleep(0)
    await st.write(Pcm(22050, 2, 1, b"\x01\x00" * 100))
    await st.write(Pcm(16000, 2, 1, b"\x02\x00" * 10))  # other format: dropped
    await st.write(Pcm(22050, 2, 1, b"\x03\x00" * 50))
    await st.close()
    await early
    late = asyncio.create_task(read())
    await late
    assert got[0] == got[1]
    pcm = from_wav(got[0])
    assert (pcm.rate, pcm.width, pcm.channels) == (22050, 2, 1)
    assert pcm.data == b"\x01\x00" * 100 + b"\x03\x00" * 50


async def test_empty_stream_is_short_silence():
    st = AudioStream()
    await st.close()
    data = b"".join([c async for c in st.chunks()])
    pcm = from_wav(data)
    assert pcm.data and set(pcm.data) == {0}


async def test_stream_over_http_grows_while_reading():
    store = AudioStore("127.0.0.1", 0, "", 60)
    await store.start()
    try:
        st, url = store.open_stream("127.0.0.1")
        assert "/tts/stream/" in url

        async def produce():
            for _ in range(3):
                await asyncio.sleep(0.05)
                await st.write(Pcm(22050, 2, 1, b"\x05\x00" * 1000))
            await st.close()

        async with aiohttp.ClientSession() as s, s.get(url) as r:
            assert r.status == 200 and r.content_type == "audio/wav"
            task = asyncio.create_task(produce())
            first = await r.content.read(44)
            assert first[:4] == b"RIFF" and not task.done()  # header before the end of audio
            body = first + await r.read()
            await task
        assert len(from_wav(body).data) == 6000
        async with aiohttp.ClientSession() as s, s.get(url + "x") as r:
            assert r.status == 404
    finally:
        await store.stop()


# ---------------- chat completions stream ----------------


@pytest.mark.parametrize("sse", [True, False])
async def test_chat_stream(sse):
    oa = FakeOpenAI(answer="Раз два три.", sse=sse)
    await oa.start()
    try:
        c = OpenAIClient(OpenAIConfig(base_url=oa.base_url, api_key="k"))
        deltas = [d async for d in c.chat_stream("m", [{"role": "user", "content": "x"}])]
        assert "".join(deltas) == "Раз два три."
        assert len(deltas) == (3 if sse else 1)
        assert oa.rec.chats[0]["stream"] is True
    finally:
        await oa.stop()


# ---------------- phrase by phrase TTS ----------------


class FakeTts:
    def __init__(self, fail_on: str = ""):
        self.texts = []
        self.fail_on = fail_on

    async def stream(self, text, voice=None):
        self.texts.append(text)
        if self.fail_on and self.fail_on in text:
            raise RuntimeError("boom")
        await asyncio.sleep(0.01)
        yield Pcm(22050, 2, 1, b"\x07\x00" * 10)
        yield Pcm(22050, 2, 1, b"\x08\x00" * 10)


async def test_streaming_answer_speaks_phrases_in_order():
    st = AudioStream()
    first = []
    tts = FakeTts(fail_on="второй")
    ans = StreamingAnswer(tts, None, st, lambda: first.append(1), min_chars=5)
    for d in ["Первый **ответ**. ", "Это второй кусок. ", "И третий"]:
        await ans.on_text(d)
    assert ans.started
    await ans.finish()
    assert tts.texts == ["Первый ответ.", "Это второй кусок.", "И третий"]
    assert first == [1]
    assert (ans.phrases, ans.failed) == (3, 1)
    assert st.done
    data = b"".join([c async for c in st.chunks()])
    assert len(from_wav(data).data) == 2 * 40  # two phrases spoken, one lost


async def test_streaming_answer_cancel_closes_the_stream():
    st = AudioStream()
    ans = StreamingAnswer(FakeTts(), None, st, lambda: None, min_chars=1)
    await ans.on_text("Раз. Два. ")
    await ans.cancel()
    assert st.done
