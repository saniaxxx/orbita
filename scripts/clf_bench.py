#!/usr/bin/env python3
"""Request classifiers for orbita on Russian voice requests: accuracy and CPU time.

Run in any venv with torch (CPU), transformers and laya:

    USE_TF=0 python clf_bench.py [regex] [rubert] [e5] [laya]

Methods: orbita's regex for the "long" mode (binary only), Laya Multilingual zero-shot (options
in the request), ruBERT-tiny2 and multilingual-e5-small nearest-centroid over 5 examples per
class. Test phrases do not overlap the examples.
"""

import os
import re
import statistics
import sys
import time

os.environ.setdefault("USE_TF", "0")
import torch  # noqa: E402

torch.set_num_threads(os.cpu_count() or 2)

CLASSES = {
    "long": "просьба рассказать развёрнуто: сказку, историю, стихи, подробный рассказ",
    "question": "короткий вопрос о фактах, числах, людях, местах",
    "home": "команда или вопрос умному дому: свет, температура, шторы, техника",
    "timer": "таймер, будильник, напоминание",
    "chat": "болтовня, приветствие, благодарность, настроение",
}

EXAMPLES = {
    "long": [
        "Расскажи сказку про лису",
        "Расскажи подробно, как работает двигатель",
        "Прочитай стихотворение про зиму",
        "Расскажи длинную историю про пиратов",
        "Объясни развёрнуто, что такое квантовая физика",
    ],
    "question": [
        "Какая столица Франции?",
        "Сколько лет Земле?",
        "Кто написал Муму?",
        "Когда началась Вторая мировая война?",
        "Сколько будет семь на восемь?",
    ],
    "home": [
        "Включи свет в гостиной",
        "Выключи телевизор",
        "Сделай температуру двадцать два градуса",
        "Закрой шторы в спальне",
        "Включи робот-пылесос",
    ],
    "timer": [
        "Поставь таймер на десять минут",
        "Разбуди меня в шесть утра",
        "Сколько осталось на таймере?",
        "Отмени будильник",
        "Напомни через час позвонить маме",
    ],
    "chat": ["Привет", "Как у тебя дела?", "Спасибо большое", "Мне грустно", "Ты кто такая?"],
}

TEST = [
    ("long", "Почитай что-нибудь на ночь"),
    ("long", "Сочини стишок про осень"),
    ("long", "Расскажи что-нибудь интересное про динозавров, хочу послушать подольше"),
    ("long", "Объясни мне всё про чёрные дыры, не торопясь"),
    ("long", "Расскажи сказку про кота"),
    ("long", "Расскажи историю создания интернета"),
    ("long", "Давай поподробнее про Древний Рим"),
    ("long", "Прочти мне басню"),
    ("long", "Расскажи развёрнуто, как устроен холодильник"),
    ("long", "Придумай рассказ про робота и девочку"),
    ("long", "Опиши подробно, как варить борщ"),
    ("long", "Расскажи мне что-нибудь длинное и доброе перед сном"),
    ("question", "Какая высота Эвереста?"),
    ("question", "Сколько дней в високосном году?"),
    ("question", "Кто изобрёл телефон?"),
    ("question", "Где находится Байкал?"),
    ("question", "Сколько будет двенадцать умножить на четыре?"),
    ("question", "В каком году был основан Петербург?"),
    ("question", "Какая самая длинная река?"),
    ("question", "Что такое фотосинтез?"),
    ("question", "Сколько весит слон?"),
    ("question", "Как зовут президента Франции?"),
    ("question", "Какой курс доллара?"),
    ("question", "Какая погода будет завтра?"),
    ("home", "Включи свет на кухне"),
    ("home", "Сделай потеплее в спальне"),
    ("home", "Выключи всё в детской"),
    ("home", "Открой шторы"),
    ("home", "Какая температура в детской?"),
    ("home", "Приглуши свет в гостиной"),
    ("home", "Запусти пылесос"),
    ("home", "Включи кондиционер"),
    ("home", "Свет в ванной выключи"),
    ("home", "Сделай ярче"),
    ("home", "Включи чайник"),
    ("home", "Закрой ворота"),
    ("timer", "Поставь таймер на пять минут"),
    ("timer", "Разбуди меня в семь"),
    ("timer", "Сколько осталось?"),
    ("timer", "Отмени таймер"),
    ("timer", "Засеки три минуты"),
    ("timer", "Напомни мне через двадцать минут выключить духовку"),
    ("timer", "Поставь будильник на завтра на восемь"),
    ("timer", "Таймер на полчаса"),
    ("timer", "Убери все будильники"),
    ("timer", "Через сколько сработает таймер?"),
    ("chat", "Мне скучно"),
    ("chat", "Доброе утро"),
    ("chat", "Спасибо, ты молодец"),
    ("chat", "Как тебя зовут?"),
    ("chat", "Я устал"),
    ("chat", "Ты меня слышишь?"),
    ("chat", "Пока"),
    ("chat", "Ха-ха, смешно"),
    ("chat", "Ты умная?"),
    ("chat", "Что ты умеешь?"),
]

# orbita/intents.py DEFAULT_MODES
LONG_RE = re.compile(r"сказк|истори|сти[хш]|подробн|поподробн|развёрнут|развернут", re.IGNORECASE)


def report(name: str, pred: list[str], times: list[float]) -> None:
    gold = [g for g, _ in TEST]
    acc = sum(p == g for p, g in zip(pred, gold, strict=True)) / len(gold)
    tp = sum(p == "long" and g == "long" for p, g in zip(pred, gold, strict=True))
    fp = sum(p == "long" and g != "long" for p, g in zip(pred, gold, strict=True))
    fn = sum(p != "long" and g == "long" for p, g in zip(pred, gold, strict=True))
    per = {c: sum(p == g == c for p, g in zip(pred, gold, strict=True)) for c in CLASSES}
    tot = {c: sum(g == c for g in gold) for c in CLASSES}
    ms = statistics.median(times) * 1000 if times else 0
    print(f"\n== {name}")
    if name != "regex":
        print(f"5 classes: {acc:.0%}  " + "  ".join(f"{c} {per[c]}/{tot[c]}" for c in CLASSES))
    print(f"long vs rest: found {tp}/{tp + fn}, false {fp}   cpu {ms:.0f} ms/request (median)")
    wrong = [
        (g, t, p)
        for (g, t), p in zip(TEST, pred, strict=True)
        if p != g and (name != "regex" or "long" in (p, g))
    ]
    for g, t, p in wrong[:12]:
        print(f"   {g:>8} -> {p:<8} {t}")


def regex() -> None:
    pred, times = [], []
    for _, text in TEST:
        t = time.perf_counter()
        pred.append("long" if LONG_RE.search(text) else "other")
        times.append(time.perf_counter() - t)
    report("regex", pred, times)


def embedder(model_id: str, prefix: str = "") -> None:
    from transformers import AutoModel, AutoTokenizer

    t = time.perf_counter()
    tok = AutoTokenizer.from_pretrained(model_id)
    model = AutoModel.from_pretrained(model_id).eval()
    load = time.perf_counter() - t

    @torch.inference_mode()
    def embed(text: str) -> torch.Tensor:
        enc = tok(prefix + text, return_tensors="pt", truncation=True, max_length=128)
        out = model(**enc).last_hidden_state
        mask = enc["attention_mask"].unsqueeze(-1)
        v = (out * mask).sum(1) / mask.sum(1)
        return torch.nn.functional.normalize(v, dim=-1)[0]

    centroids = {
        c: torch.nn.functional.normalize(torch.stack([embed(x) for x in xs]).mean(0), dim=0)
        for c, xs in EXAMPLES.items()
    }
    embed("прогрев")
    pred, times = [], []
    for _, text in TEST:
        t = time.perf_counter()
        v = embed(text)
        pred.append(max(centroids, key=lambda c: float(v @ centroids[c])))
        times.append(time.perf_counter() - t)
    report(f"{model_id} (load {load:.1f} s)", pred, times)


def laya_ml() -> None:
    import laya

    t = time.perf_counter()
    agent = laya.load("convaiinnovations/laya", subfolder="multilingual")
    load = time.perf_counter() - t
    q = {
        "kind": {
            "type": "choice",
            "instructions": "Что просит пользователь голосового ассистента?",
            "criteria": CLASSES,
        }
    }
    agent.predict({"body": "прогрев"}, q)
    pred, times = [], []
    for _, text in TEST:
        t = time.perf_counter()
        r = agent.predict({"body": text}, q)
        times.append(time.perf_counter() - t)
        pred.append(r["answers"]["kind"]["choice"])
    report(f"laya-multilingual zero-shot (load {load:.1f} s)", pred, times)


if __name__ == "__main__":
    print(f"test: {len(TEST)} phrases, examples: 5 per class, threads {torch.get_num_threads()}")
    for m in sys.argv[1:] or ["regex", "rubert", "e5", "laya"]:
        if m == "regex":
            regex()
        elif m == "rubert":
            embedder("cointegrated/rubert-tiny2")
        elif m == "e5":
            embedder("intfloat/multilingual-e5-small", "query: ")
        elif m == "laya":
            laya_ml()
