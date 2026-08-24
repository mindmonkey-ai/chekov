# Hugging Face Model Selection Guide

A complete, exhaustive guide to choosing a model on Hugging Face, understanding
quantization ("quants"), and the vocabulary that fills model pages. Written for
local inference (llama.cpp / GGUF) but the terms apply across the whole Hub.

---

## Table of Contents

1. [What Hugging Face actually is](#1-what-hugging-face-actually-is)
2. [The core vocabulary (read this first)](#2-the-core-vocabulary-read-this-first)
3. [Model anatomy: what a "model" file really contains](#3-model-anatomy-what-a-model-file-really-contains)
4. [Base vs. Instruct (chat-tuned) models](#4-base-vs-instruct-chat-tuned-models)
5. [Model families / architectures](#5-model-families-architectures)
6. [Choosing the right model — decision guide](#6-choosing-the-right-model--decision-guide)
7. [Quantization explained (the "quants")](#7-quantization-explained-the-quants)
8. [Quant selection cheat sheet](#8-quant-selection-cheat-sheet)
9. [File formats: GGUF, safetensors, PyTorch, etc.](#9-file-formats-gguf-safetensors-pytorch-etc)
10. [Sampling / generation parameters](#10-sampling--generation-parameters)
11. [Hardware & inference terms](#11-hardware--inference-terms)
12. [Tokens, vocab, and tokenization](#12-tokens-vocab-and-tokenization)
13. [Fine-tuning vocabulary: LoRA, adapters, checkpoints](#13-fine-tuning-vocabulary-lora-adapters-checkpoints)
14. [License & safety vocabulary](#14-license--safety-vocabulary)
15. [Reading a Hugging Face model page](#15-reading-a-hugging-face-model-page)
16. [Real-world worked examples](#16-real-worked-examples)
17. [Glossary (alphabetical)](#17-glossary-alphabetical)

---

## 1. What Hugging Face actually is

Hugging Face (HF, huggingface.co) is a hub for sharing machine-learning
artifacts. For a local-inference user, the things you actually interact with:

- **Model** — a repo that holds the model's weights plus metadata. Think of it
  as a folder on the internet.
- **Dataset** — the data a model was trained on (rarely downloaded by end users).
- **Space** — a hosted web app (like a demo of a model). Not weights.
- **Pipeline** — HF Transformers code that wraps a model for a task (text
  generation, image classification, etc.). You mostly won't need this for
  llama.cpp.

Every model lives at a URL shaped like:

```
https://huggingface.co/<ORG>/<REPO-NAME>
```

Examples:
- `https://huggingface.co/meta-llama/Llama-3.1-8B`
- `https://huggingface.co/bartowski/Qwen2.5-7B-Instruct-GGUF`

`meta-llama` is the **organization** (or a person's username). `Llama-3.1-8B`
is the **repo**. The **repo id** is the full `org/repo` string — you'll paste it
into tools.

### Auth & access

- Some models are **open** (anyone can download).
- Some require **accepting a license** by clicking on the HF page before you can
  download (e.g. Llama models). You must be logged in and accept it.
- `hf download` / tools will ask you to authenticate if a repo is gated.

---

## 2. The core vocabulary (read this first)

These 10 terms cover most of what you'll see. Learn them, the rest is detail.

| Term | Plain meaning |
|------|---------------|
|| [Model / checkpoint](#checkpoint) | The saved weights — the "brain" of the AI. |
| [Parameters (params)](#parameters) | The number of numbers in the brain. More usually = smarter but heavier. |
| [Weight](#weight) | One number in the network — the individual learned values. A model with 7B params has 7 billion weights. |
| [Bits per weight](#bits-per-weight) | How much memory each weight takes. Fewer bits = smaller file, lower quality. This is what "Q4" and "FP16" refer to. |
| [FP16](#fp16) / [BF16](#bf16) | The original high-precision (16-bit) format weights are trained and stored in. Big and lossless. |
| [Quantization (quant)](#quantization) | Compressing the weights to use less memory at some quality cost. |
| [GGUF](#gguf) | The file format llama.cpp uses to load quantized models. |
| [Token](#token) | A chunk of text (sometimes a word, sometimes part of a word) the model reads. |
| [Context length (n_ctx)](#context-length) | How much text the model can "see" at once. |
| [VRAM](#vram) / RAM | Memory. VRAM is GPU memory (fast, small); RAM is system memory (slower, bigger). |
| [Inference](#inference) | Running the model to produce output (a.k.a. "serving" or "generating"). |
| [Perplexity](#perplexity) | A quality number — lower is better. |
| [Sampling](#sampling) params | Knobs like temperature that change how output is generated. |

---

## 3. Model anatomy: what a "model" file really contains

A large language model (LLM) is a neural network. Its "size" is measured in
**parameters** — the adjustable numbers learned during training.

```
1 parameter ≈ 1 connection weight in the network
```

### Parameters → file size (the 4-bit rule of thumb)

At 4 bits per parameter, a model takes roughly **0.5 GB per billion
parameters**. This is the single most useful mental model for predicting file
sizes:

| Parameters | FP16 (~16-bit) | Q8_0 (~8-bit) | Q4_K_M (~4-bit) |
|-----------:|--------------:|--------------:|----------------:|
| 1B         | ~2 GB         | ~1.1 GB       | ~0.6 GB         |
| 3B         | ~6 GB         | ~3.3 GB       | ~1.8 GB         |
| 7B         | ~14 GB        | ~7.5 GB       | ~4.3 GB         |
| 8B         | ~16 GB        | ~8.5 GB       | ~4.9 GB         |
| 13B        | ~26 GB        | ~14 GB        | ~8 GB           |
| 32B        | ~64 GB        | ~34 GB        | ~19 GB          |
| 70B        | ~140 GB       | ~75 GB        | ~42 GB          |
| 405B       | ~810 GB       | ~430 GB       | ~240 GB         |

**Why this matters:** your hardware limits which models you can run. A model
must fit in VRAM (+ some RAM) to run at a good speed. This guide's rest is
mostly about fitting a model into your memory.

### Dense vs. MoE (Mixture of Experts)

- [Dense](#dense) — every parameter is used for every token. Simpler, predictable.
  Most "7B", "70B" models are dense.
- [MoE](#moe) — only a subset of "experts" activate per token. Can be much larger
  in total params but cheaper to run. Example: Qwen3-235B-A22B means 235B total
  params, 22B active per token.

```
Dense:   [ALL params] → each token
MoE:     [expert1 expert2 expert3 ...] → only ~22B active per token
```

---

## 4. Base vs. Instruct (chat-tuned) models

Two flavors of almost every model:

| Type | What it does | When to pick |
|------|--------------|--------------|
| **Base** | Completes text. You prompt: `"The capital of France is"` → it writes `"Paris, and..."` | Fine-tuning, research, when you want full control over behavior. |
| **Instruct / Chat** | Trained to follow instructions and hold a conversation. Responds to roles (`system`, `user`, `assistant`). | Almost everything. This is what you want for a chatbot/assistant. |

How to tell them apart on a page:
- Repo name usually says **`Instruct`**, **`Chat`**, or **`I18N`**.
  - `Llama-3.1-8B-Instruct` → chat-tuned
  - `Llama-3.1-8B` (no suffix) → base
- Base models often **refuse to answer** or just continue your sentence instead
  of responding — that's expected, not a bug.

**Rule of thumb:** pick the **Instruct** version unless you have a specific
reason (fine-tuning, custom prompting) to use base.

---

## 5. Model families / architectures

Different "families" are built by different organizations and have different
strengths. You'll see these names a lot:

| Family | Creator | Notes |
|--------|---------|-------|
| **Llama 3.x** | Meta | Most popular open family. Strong all-rounder. |
| **Qwen 2.5 / 3** | Alibaba | Excellent multilingual + code. Very active. |
| **Mistral / Mixtral** | Mistral AI | Efficient European models. Mixtral is MoE. |
| **Gemma 2/3** | Google | Lightweight, efficient. Gemma is good on laptops. |
| **Phi-3 / Phi-4** | Microsoft | Small but surprisingly strong (Microsoft's "small but mighty"). |
| **DeepSeek** | DeepSeek | Strong reasoning + code, often free. |
| **Grok** | xAI | Full-size open models. |
| **Cohere Command** | Cohere | Good for enterprise/RAG. |
| **Yi** | 01.AI | Strong multilingual. |

**Architecture-specific terms you'll see:**
- [Attention](#attention) — how a model weighs different parts of its input.
- [KV cache](#kv-cache) — cached attention data; size affects how much context you can
  hold and how much VRAM it needs.
- [GQA](#gqa) (Grouped-Query Attention) — a memory-saving attention variant.
- **Sliding window attention** — lets large models use long context cheaply.
- [Rotary embeddings (RoPE)](#rope) — how position is encoded; affects context length.

---

## 6. Choosing the right model — decision guide

Work down this decision tree.

```
START: What do I want to run?
  │
  ├─ On a laptop / limited RAM?
  │    → Small model (≤7B) + lighter quant (Q4_K_M / Q3)
  │
  ├─ On a GPU with lots of VRAM?
  │    → Bigger model + higher quant (Q5/Q6/Q8)
  │
  ├─ Need code?
  │    → Qwen2.5-Coder, DeepSeek-Coder, Llama with code checkpoint
  │
  ├─ Need multilingual?
  │    → Qwen2.5, Llama 3.1 (128k langs), Gemma
  │
  ├─ Need long documents (RAG)?
  │    → Model with large context (128k/1M tokens) + enough VRAM for KV cache
  │
  └─ Just want a capable assistant?
       → Qwen2.5-7B-Instruct or Llama-3.1-8B-Instruct, Q4_K_M
```

### Quick recommendation table by use case

| Use case | Recommended model | Recommended quant | Why |
|----------|-------------------|-------------------|-----|
| General assistant (laptop) | Qwen2.5-7B-Instruct | Q4_K_M | Fits ~4.3 GB, strong all-rounder |
| General assistant (desktop GPU) | Llama-3.1-8B-Instruct | Q6_K | Near-lossless quality |
| Code generation | Qwen2.5-Coder-7B-Instruct | Q5_K_M | Code-tuned, higher precision helps |
| Creative writing | Llama-3.1-8B-Instruct | Q4_K_M | Good style, runs widely |
| Long-context / RAG | Qwen2.5-7B-Instruct-1M | Q4_K_M | 1M token context window |
| Very low RAM (≤8 GB) | Phi-4 / Gemma 3 | Q3_K_M / Q4 | Smallest footprint |
| Multilingual | Qwen2.5-7B-Instruct | Q4_K_M | Strong in many languages |
| Reasoning | DeepSeek-R1 / Qwen3 | Q5_K_M | Better logic at higher precision |

### The one-line decision rule

> **Fit the model into your memory, then spend remaining quality budget on the
> highest quant you can.** A model that doesn't fit runs too slowly to be
> useful; a slightly lower quant that still fits is better than an unrunnable
> perfect-quality file.

---

## 7. Quantization explained (the "quants")

### The core idea

Training a model computes weights as high-precision floating-point numbers
(usually **FP16** or **BF16**, 16 bits each). **Quantization** shrinks those
numbers to fewer bits so the file is smaller and runs faster — at a small loss
of quality.

```
FP16 (16 bits)  →  Q8 (8 bits)  →  Q4 (4 bits)  →  Q2 (2 bits)
   big / best        half / good     quarter / small     tiny / lossiest
```

> **The bits-per-weight nuance.** The number in a quant name is *roughly* the
> bits per weight, but K-quants use mixed precision, so the real number is a
> bit different: `Q4_K_M` is actually **~4.5 bits/weight**, `Q5_K_M` is
> **~5.5**, `Q6_K` is **~6.6**, and `Q8_0` is **~8**. The label is a name, not a
> precise spec. `Q8_0` is the only one that's truly 8 bits — that's why it's the
> reference for "near-lossless."

### The photo-resolution analogy

Think of a model's weights as a digital photograph.

```
FP16 (16-bit)  =  a 4K RAW image — full detail, huge file
Q8_0  (8-bit)  =  a high-quality JPEG — looks identical to the eye, half the size
Q6_K  (6-bit)  =  a very good JPEG — only an expert notices the difference
Q4_K_M(4-bit)  =  a standard JPEG — great for everyday viewing, tiny file
Q2_K  (2-bit)  =  a heavily compressed, blurry JPEG — you can see the artifacts
```

Quantization is like choosing an image-compression level. You lose a little
detail, but for most people at most levels the picture looks the same. The
"artifacts" in a model show up as slightly worse reasoning, rarer vocabulary, or
occasional hallucinations — and they appear first at the aggressive end (Q2/Q3).

### Bits vs. size vs. quality — the trade-off

There is a fundamental tension:

```
Fewer bits  →  smaller file  +  faster  +  less memory   BUT   lower quality
More bits   →  bigger file  +  slower  +  more memory      BUT   higher quality
```

### The quant naming scheme decoded

The best way to read a quant name like `Q4_K_M` is to break it into its four
pieces:

```
Q4_K_M
| | |  | | +--- M = size variant (S = small, M = medium, L = large)
| | |  | +----- K = a smarter "K-quant" method (mixed precision)
| | |  +------- 4 = roughly 4 bits used per weight
| | +---------- K = "K-quant" family
| +------------ 4 = bits per weight
+-------------- Q = "Quantized"
```

Read it left to right: **Q**uantized, ~**4** bits per weight, using the **K**
mixed-precision method, in the **M**edium size. The same logic decodes any name:
`Q6_K` = quantized, ~6 bits, K-quant (no size suffix → the single K-variant for
that bitrate); `Q8_0` = quantized, 8 bits, "0" is the non-K block format.

Other suffixes you'll see:
- `_S` (small) — smaller, slightly lower quality, faster.
- `_M` (medium) — the balanced default. **This is what most repos recommend.**
- `_L` (large) — better quality, larger file.
- `_0`, `_1` (e.g. `Q8_0`, `Q5_0`) — non-K "legacy" formats; `_0` is the
  reference quality quant.

### The main quant families

There are two families of "K-quant":

| Prefix | Family | Character |
|--------|--------|-----------|
| `Q` | Standard llama.cpp quants (Q2–Q6) | Broad, well-tested range. |
| `IQ` (e.g. `IQ4_XS`, `IQ3_XXS`) | "Improved Quantization" | Smaller files at similar quality, especially at low bitrates. |
| `UD` / `UF` | Unsloth dynamic/ultra-fine quants | Community quants, often very small with good quality. |
| `Q8_0`, `Q5_0`, `Q5_1` | Non-K "legacy" quants | Simpler, older format; Q8_0 is the quality reference. |

### Quant comparison chart (perplexity & size, ~7B reference)

Perplexity is measured against the model's original FP16. A lower number is
better; the % shows how much worse than the original.

| Quant | Approx. size (7B) | Memory (7B) | Perplexity delta | Quality feel | Verdict |
|-------|------------------:|------------:|------------------|--------------|---------|
| FP16  | ~14 GB  | ~14 GB  | baseline (0%)      | Reference    | Only if you have the RAM and don't care about size |
| **Q8_0**  | ~7.5 GB | ~8 GB   | +0.03%           | Nearly lossless | Best quality you can afford |
| **Q6_K**    | ~5.5 GB | ~6 GB   | +0.13%           | Excellent      | Best quality/size for capable GPUs |
| **Q5_K_M**  | ~4.8 GB | ~5 GB   | +0.39%           | Very good      | Great when you have a little extra room |
| **Q4_K_M**  | ~4.3 GB | ~5 GB   | +1.7%            | Good           | **The default recommendation** |
| Q4_K_S  | ~4.0 GB | ~4.5 GB | +2.6%            | Good           | Smaller than Q4_K_M, slightly lower quality |
| Q3_K_M  | ~3.3 GB | ~4 GB   | +6%              | Acceptable     | Tight RAM; small models only really |
| IQ4_XS  | ~3.9 GB | ~4.5 GB | ~Q4 level        | Good           | Smaller-file alternative to Q4 |
| Q2_K    | ~2.7 GB | ~3 GB   | +15%             | Noticeably worse | Only as last resort |

> **Takeaway:** `Q4_K_M` is the sweet spot for most people. Move up to `Q6_K` /
> `Q8_0` if you have the VRAM; move down to `Q3` / `IQ` variants if you're
> memory-starved.

### Per-sample vs. block quantization

- **Per-sample quants** (`Q4_0`, `Q5_0`) — quantize each row independently.
- **Block quants** (`Q4_K_M`) — group weights into blocks and share a scale.
  K-quants use block quantization with **mixed precision** (attention gets more
  bits than feed-forward layers). This is why `Q4_K_M` sounds much better than
  the "4" in its name would suggest.

### imatrix (importance matrix)

Some quants are built using an [imatrix](#imatrix) — a calibration of which
weights matter most for a given domain. This can improve quality 10–20% at the same
bitrate, and is essential for Q3 and below. Community repos (like Unsloth's
`UD` quants) often use this.

---

## 8. Quant selection cheat sheet

Use this when a repo doesn't tell you directly.

| Your constraint | Pick |
|-----------------|------|
| Just want it to work well | **Q4_K_M** |
| Have a GPU with 12+ GB free VRAM | Q6_K or Q8_0 |
| Want best quality and don't mind size | Q8_0 |
| Code / technical work, memory allows | Q5_K_M or Q6_K |
| Laptop, ≤8 GB total | Q3_K_M or IQ variants |
| Very tight (≤4 GB) | Q2_K or IQ3_XXS (accept quality loss) |
| Multimodal repo (vision) | Get the `mmproj-*.gguf` projector separately — it's not the main model |

**Never normalize repo-native labels.** If a page says `UD-Q4_K_M` or
`IQ4_NL_XL`, that's the exact filename — report and use it as-is.

---

## 9. File formats: GGUF, safetensors, PyTorch, etc.

You'll encounter several file formats on the Hub. Know which you need.

| Format | Extension | Used by | You need it if… |
|--------|-----------|---------|-----------------|
| **GGUF** | `.gguf` | llama.cpp, MLX, llama.cpp-based tools | Running locally on CPU/Apple Silicon/GPU via llama.cpp |
|| [safetensors](#safetensors) | `.safetensors` | HF Transformers, PyTorch | Running via Python/Transformers (GPU-heavy) |
| **PyTorch** | `.bin` / `.pt` | Older Transformers | Running via Python (older setups) |
| **Sharded** | `*.safetensors.index.json` | Large models split across files | Downloading a huge model (needs the index) |

### For local inference (this repo's focus)

You almost always want a **GGUF** file. Repos built for llama.cpp end their
repo name in `-GGUF` (e.g. `.../Qwen2.5-7B-Instruct-GGUF`) and contain one or
more `.gguf` files, sometimes alongside:
- `mmproj-*.gguf` — a vision **projector** for multimodal models (not the main
  model; load it separately for image support).
- `BF16/` — unquantized shards (huge; rarely needed).

### How to find the GGUFs in a repo

1. Open the repo's llama.cpp local-app page:
   `https://huggingface.co/<repo>?local-app=llama.cpp`
   — it shows the recommended quant and a ready-to-run command.
2. Confirm exact files and sizes via the tree API:
   `https://huggingface.co/api/models/<repo>/tree/main?recursive=true`
   — keep entries where the path ends in `.gguf`.

---

## 10. Sampling / generation parameters

Once a model is loaded, these knobs control **how** it generates text. They are
NOT stored in the weights — they're set at run time.

| Parameter | Range | What it does | Practical guidance |
|-----------|-------|--------------|--------------------|
| **Temperature** | 0–2 (typ. 0.2–1.0) | Randomness / creativity. Low = focused/deterministic; high = wild/creative. | 0.2–0.7 for facts/code; 0.8–1.2 for creative writing; never 0 for "varied" output. |
| **Top-p (nucleus)** | 0–1 | Limits sampling to the smallest set of tokens whose probability sums to p. | 0.9 is a good default; lower = safer. |
| **Top-k** | 0–100+ | Restricts to the k most likely next tokens. | Often redundant with top-p; 40–70 if used. |
| **Repetition penalty** | 1.0–1.5 | Penalizes repeated tokens. 1.0 = off. | 1.1–1.2 if output loops. |
| **Max tokens (max_new_tokens)** | int | How many tokens to generate. | Set a cap so generation doesn't run forever. |
| **Context length (n_ctx)** | int | Max input+output tokens the model holds. | Must fit in memory; bigger = more VRAM. |
| **Stop sequences** | string | Text that halts generation (e.g. `</s>`, `\n\n`). | Prevents rambling / leaks "assistant:" prefixes. |
| **Seed** | int | Fixes randomness for reproducible output. | Set it to get the same answer every run. |

### The temperature mental model

```
Temperature ≈ 0.0   →  robotic, predictable, best for math/code
Temperature ≈ 0.7   →  balanced, "normal"
Temperature ≈ 1.0+  →  creative, surprising, may hallucinate
```

---

## 11. Hardware & inference terms

| Term | Meaning |
|------|---------|
| **VRAM** | Video memory on your GPU. The hard limit for how much of the model can run on the GPU. |
| **RAM** | System memory. Models can run here but slower than VRAM. |
| **ngl / --nl / n_gpu_layers** | Number of model layers offloaded to the GPU. `0` = CPU only; `99` (or `-1`) = everything on GPU. |
| **n_threads / --threads** | CPU cores used for inference. Usually match your physical cores. |
| **Batch size (n_batch)** | How many tokens processed per forward step. Bigger = faster but more memory. |
| **KV cache** | Cached attention state. Grows with context length and batch. A major VRAM consumer for long context. |
| **Tokens/sec (tok/s)** | Speed of generation. Higher = snappier. |
| **Prefill** | Processing your input prompt. |
| **Decode** | Generating each output token one at a time (the slow part). |
| **CPU offload** | Running parts of the model on CPU when it doesn't fit in VRAM. |
| **Metal** | Apple's GPU framework; llama.cpp uses it for Apple Silicon speed. |
| **CUDA / ROCm** | NVIDIA and AMD GPU compute frameworks. |
| **Flash attention** | A memory-efficient attention algorithm that speeds things up. |

### VRAM budgeting (the practical part)

```
Free VRAM available:  12 GB
Model (Q4_K_M, 8B):   ~5 GB
KV cache @ 8k ctx:    ~3 GB
Overhead:              ~1 GB
─────────────────────────────
Fits comfortably:  YES → run with -ngl 99 (all layers on GPU)

If it does NOT fit:
  - lower the quant (Q4 → Q3)
  - reduce context length (-c)
  - offload fewer layers (-ngl)
```

---

## 12. Tokens, vocab, and tokenization

### What is a token?

A **token** is the unit a model reads. It is *not* a word. Common split:

```
"The cat sat on the mat!"
→ ["The", " cat", " sat", " on", " the", " mat", "!"]
≈ 7 tokens for a 26-character sentence
```

Rough rule of thumb: **1 token ≈ 0.75 words** (≈ 4 characters).

### Vocabulary (vocab)

The **vocabulary** is the model's dictionary — the full set of tokens it knows.
- GPT-2: ~50k tokens
- Llama 3: ~128k tokens
- Qwen2.5: ~150k tokens

A bigger vocab generally means fewer tokens per document (more efficient) and
better handling of rare words/unicode.

### Tokenization methods

| Method | Idea | Example families |
|--------|------|------------------|
| **BPE** (Byte-Pair Encoding) | Merges frequent byte pairs into tokens. | Llama 3, Qwen, Mistral |
| **WordPiece** | Known-word tokens, `##` marks continuations. | BERT, some older models |
| **Byte-level BPE** | Operates on bytes; handles any language. | GPT-2, GPT-3 |
| **SentencePiece** | Unicode-aware, language-agnostic. | CodeLlama, some Gemma |

### Why tokens matter to you

- **Billing / limits** are per-token (relevant for APIs).
- **Context length** is measured in tokens — know how many tokens your document
  is before assuming it fits your `-c` setting.
- **A token limit is a hard wall:** prompt tokens + generated tokens must stay
  under `n_ctx`. Long prompts eat context fast.

### Counting tokens in your document

```bash
# If you have the tiktoken-style tooling:
python -c "from transformers import AutoTokenizer; \
t=AutoTokenizer.from_pretrained('Qwen/Qwen2.5-7B-Instruct'); \
print(len(t.encode(open('doc.txt').read())))"
```

---

## 13. Fine-tuning vocabulary: LoRA, adapters, checkpoints

If you see these on a page, here's what they mean:

| Term | Meaning |
|------|---------|
|| [Checkpoint](#checkpoint) | A saved point in training (e.g. `checkpoint-500`). Sometimes a partially-trained model you can use. |
|| [Fine-tune](#fine-tune) | Continuing training a model on new data to specialize it. |
|| [LoRA](#lora) (Low-Rank Adaptation) | A tiny file that "adapts" a base model for a task without retraining everything. |
|| [Adapter](#adapter) | Same idea as LoRA — a small additive module. |
| **PEFT** | HF library for applying LoRA/adapters easily. |
| **Full fine-tune** | Retraining all parameters (expensive; large). |
| **Checkpoint merging** | Fusing a LoRA into the base weights so you get one model file. |

LoRA files are small (MBs) vs. a full checkpoint (GBs). You attach a LoRA to a
base model at inference time — you don't run a LoRA alone.

---

## 14. License & safety vocabulary

Reading the license matters — "open" is not one thing.

| Term | What it means |
|------|---------------|
| **Apache 2.0 / MIT / MPL 2.0** | Permissive; usually free for personal + commercial use. |
| **Llama Community License** | Free under 700M users revenue; restrictions on commercial use above that. |
| **Commercial use** | Using the model to make money or in a product. Often the thing licenses restrict. |
| **Research-only** | Not allowed for commercial products. |
| **Attribution (BY)** | You must credit the creator. |
| **ShareAlike** | Derivatives must use the same license. |
| **Non-commercial (NC)** | Personal/research only, no money. |
| **Safety / modality** | Some models are flagged for content moderation, adult content, etc. |
| **Accept license** | A gate on the HF page you must click before downloading. |

**Always read the model card's license section** before using a model
commercially.

---

## 15. Reading a Hugging Face model page

A typical model page has these sections:

```
┌─────────────────────────────────────────────────┐
│  Model title, org, downloads, likes             │
│  [Tags: task, language, library, quant]         │
├─────────────────────────────────────────────────┤
│  Model card  → README explaining what it is     │
│  License     → the legal terms                  │
│  Files & versions → the actual weight files     │
│  Config      → architecture, context, vocab size│
│  Metrics     → benchmark scores (MMLU, etc.)    │
│  Q&A / Community → questions & discussions      │
└─────────────────────────────────────────────────┘
```

**What to check, in order:**
1. **Config.json** (or the Config tab): `model_type`, `context_length`,
   `vocab_size`, `n_layers`, `n_heads`.
2. **Files** → find `.gguf` for local inference.
3. **License** → confirm what you're allowed to do.
4. **Model card** → known limitations, intended use.

### Useful benchmark numbers you'll see

| Metric | What it measures |
|--------|------------------|
| **MMLU** | Multidomain knowledge & reasoning. |
| **HumanEval** | Code generation quality. |
| **GSM8K** | Math word-problem solving. |
| **WinoGrande** | Common-sense reasoning. |
| **Perplexity** | Prediction quality (lower better). |

Higher scores = better on that axis, but treat them as guidance, not gospel.

---

## 16. Real-worked examples

### Example A: Choosing & running a model on a MacBook (Apple Silicon)

You have a MacBook with 16 GB unified memory.

1. **Pick a model:** `Qwen2.5-7B-Instruct` (capable, fits comfortably).
2. **Find GGUFs:** open
   `https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF?local-app=llama.cpp`
3. **Pick a quant:** `Q4_K_M` (~4.3 GB) leaves room for context + OS.
4. **Run it directly from the Hub:**

```bash
llama-server -hf Qwen/Qwen2.5-7B-Instruct-GGUF:Q4_K_M
```

5. **Test it** (OpenAI-compatible endpoint):

```bash
curl http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [
      {"role": "user", "content": "Write a limerick about Python exceptions"}
    ]
  }'
```

### Example B: You have an NVIDIA GPU with 12 GB VRAM and want best quality

```bash
# Llama-3.1-8B in Q6_K (~5.5 GB) leaves VRAM for context/KV cache
llama-server \
  --hf-repo bartowski/Llama-3.1-8B-Instruct-GGUF \
  --hf-file Llama-3.1-8B-Instruct-Q6_K.gguf \
  -ngl 99 \
  -c 8192
```

### Example C: You're memory-starved (≤8 GB)

```bash
# Phi-4 in a small IQ quant to fit tight RAM
llama-server -hf microsoft/Phi-4-GGUF:IQ3_XXS -c 4096
```

### Example D: Reading the tree API to get an exact filename

```bash
curl -s "https://huggingface.co/api/models/unsloth/Qwen2.5-7B-Instruct-GGUF/tree/main?recursive=true" \
  | python -c "import sys,json; [print(f['path'], f['size']) for f in json.load(sys.stdin) if f['path'].endswith('.gguf')]"
```

This lists every `.gguf` file and its byte size — the source of truth for
filenames (repo-native labels like `UD-Q4_K_M` become exact paths here).

### Example E: Vision (multimodal) model — the projector is separate

A multimodal repo ships two kinds of files:

```
Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf   ← the main model (text + understanding)
Qwen2.5-VL-7B-Instruct-mmproj-*.gguf ← the image projector (attach for images)
```

The `mmproj` is **not** the model — it's an adapter that lets the model "see"
images. Load it only when you need image input.

---

## 17. Glossary (alphabetical)

- <a id="adapter"></a>**Adapter** — a small additive module (see [LoRA](#lora)) that specializes a model.
- <a id="bits-per-weight"></a>**Bits per weight** — how much memory a single weight takes; the "bits" in a quant name. Fewer bits = smaller file, lower quality. K-quants are mixed precision, so `Q4_K_M` is really ~4.5 bits/weight.
- <a id="attention"></a>**Attention** — mechanism letting a model weigh input parts relative to each other. See also [GQA](#gqa) and [KV cache](#kv-cache).
- <a id="bf16"></a>**BF16** — Bfloat16, a 16-bit floating point format (common for weights). The other half of the "FP16 / BF16" pair.
- <a id="bpe"></a>**BPE** — Byte-Pair Encoding, a tokenization method (see [Token](#token) and [Tokenization](#tokenization)).
- <a id="checkpoint"></a>**Checkpoint** — a saved training state / model snapshot. Built from [Parameters](#parameters) and [Weights](#weight).
- <a id="context-length"></a>**Context length (n_ctx)** — max [tokens](#token) the model holds at once.
- <a id="dense"></a>**Dense** — a model where all parameters run on every token. The common case; see [MoE](#moe) for the alternative.
- <a id="fp16"></a>**FP16** — 16-bit floating point; the usual full-precision weight format. Quantizing down from FP16 is what makes a model smaller.
- <a id="fine-tune"></a>**Fine-tune** — continued training to specialize a model. [LoRA](#lora) is the cheap version of this.
- <a id="gguf"></a>**GGUF** — GPT-Generated Unified Format (sometimes "Generic"). llama.cpp's model file format. It packs the quantized [weights](#weight), the [vocabulary](#vocab), and all the config the loader needs into one self-contained file — that's why a GGUF "just runs" in llama.cpp with no extra pieces. You'll spot it as `*.gguf` files and in `-GGUF` repo names.
- <a id="gqa"></a>**GQA** — Grouped-Query Attention, a memory-efficient [Attention](#attention) variant that shrinks the [KV cache](#kv-cache).
- <a id="instruct"></a>**Instruct** — a chat/instruction-tuned model (vs. [Base](#base)).
- <a id="iq"></a>**IQ** — Improved Quantization; small-file quants (e.g. `IQ4_XS`). A [Quantization](#quantization) family.
- <a id="imatrix"></a>**imatrix** — importance matrix used to improve [quantization](#quantization) quality (10–20% at the same bitrate; essential for Q3 and below).
- <a id="kv-cache"></a>**KV cache** — cached attention state; grows with [context length](#context-length) and is a major [VRAM](#vram) consumer.
- <a id="lora"></a>**LoRA** — Low-Rank Adaptation; a tiny file (MBs) that adapts a base model for a task without retraining everything.
- <a id="moe"></a>**MoE** — Mixture of Experts; only some parameters activate per token. More total [parameters](#parameters) but cheaper to run than [Dense](#dense).
- <a id="n-batch"></a>**n_batch** — how many [tokens](#token) are processed per forward step.
- <a id="n-ctx"></a>**n_ctx** — [context length](#context-length) in tokens.
- <a id="ngl"></a>**n_gpu_layers / ngl** — how many model layers are offloaded to the [GPU](#vram).
- <a id="n-threads"></a>**n_threads** — CPU cores used for inference.
- <a id="parameters"></a>**Parameters** — the trainable numbers in a model; a 7B model has 7 billion of them (each one is a [Weight](#weight)).
- <a id="perplexity"></a>**Perplexity** — a quality metric; lower is better. Measured against the original [FP16](#fp16).
- <a id="prefill"></a>**Prefill** — processing your input prompt (see [Decode](#decode)).
- <a id="base"></a>**Base** — an un-tuned model that completes text (vs. [Instruct](#instruct)).
- <a id="quantization"></a>**Quantization** — shrinking [weights](#weight) to fewer [bits](#bits-per-weight) to make a model smaller and faster. See the [photo analogy](#7-quantization-explained-the-quants).
- <a id="rep-penalty"></a>**Repetition penalty** — discourages repeated output.
- <a id="safetensors"></a>**safetensors** — a fast, safe weight container format used by HF Transformers; the main alternative to [GGUF](#gguf) for Python-based inference.
- <a id="seed"></a>**Seed** — fixes randomness for reproducible output.
- <a id="stop-seq"></a>**Stop sequence** — text that halts generation.
- <a id="temperature"></a>**Temperature** — controls output randomness (part of [Sampling](#sampling)).
- <a id="token"></a>**Token** — a unit of text the model reads (~0.75 words). See [Tokenization](#tokenization).
- <a id="top-k"></a>**top-k** — restricts [Sampling](#sampling) to the k most likely tokens.
- <a id="top-p"></a>**top-p** — nucleus [Sampling](#sampling) threshold.
- <a id="vram"></a>**VRAM** — GPU memory; the hard limit for how much of a model can run on the GPU.
- <a id="vocab"></a>**Vocabulary (vocab)** — the full set of [tokens](#token) a model knows.
- <a id="weight"></a>**Weight** — one number in the network; a 7B model has 7 billion weights.
- <a id="decode"></a>**Decode** — generating each output [token](#token) one at a time (the slow part; see [Prefill](#prefill)).
- <a id="inference"></a>**Inference** — running the model to produce output (a.k.a. "serving").
- <a id="tokenization"></a>**Tokenization** — splitting text into [tokens](#token); the method (BPE, WordPiece, etc.) defines the [vocabulary](#vocab).
- <a id="sampling"></a>**Sampling** — how output tokens are chosen at run time: [temperature](#temperature), [top-p](#top-p), [top-k](#top-k).
- <a id="rope"></a>**RoPE** — Rotary Position Embeddings; how a model encodes token position and thus its max [context length](#context-length).
- <a id="q-presets"></a>**Q8_0 / Q6_K / Q5_K_M / Q4_K_M / Q3_K_M / Q2_K** — [quant](#quantization) presets from highest to lowest quality (and roughly size).

---

## TL;DR — the essential playbook

1. **Pick the model for the task** (general → Qwen2.5-7B-Instruct or
   Llama-3.1-8B-Instruct; code → a *-Coder variant; long docs → a large-context
   model).
2. **Pick Instruct, not base**, unless fine-tuning.
3. **Fit it to your memory first.** Your VRAM/RAM is the hard limit.
4. **Start at Q4_K_M.** Go up to Q6_K/Q8_0 if you have VRAM; go down to Q3/IQ
   if you're tight.
5. **Find the exact GGUF** via the repo's `?local-app=llama.cpp` page or the
   tree API — don't guess filenames.
6. **Set sampling sensibly:** temperature ~0.7, top-p ~0.9, a repetition
   penalty only if output loops.
7. **Read the license** before commercial use.
