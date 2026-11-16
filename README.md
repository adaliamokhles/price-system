# price-system

**TLB Infinitum** — live market prices travelling over a CAN bus, on an embedded Linux device.

Three small Rust programs. One downloads prices from the internet and puts them on a
CAN bus; the others read that bus and show the prices, in a terminal and in a browser.

This repository holds **only the source code**. The embedded Linux image that runs it is
built from the Yocto layer in
[meta-mokhles](https://github.com/adaliamokhles/meta-mokhles).

---

## How it fits together

```
   internet                 CAN bus (vcan0)              you
┌──────────────┐        ┌────────────────────┐     ┌──────────────┐
│  Coinbase    │        │ 0x0F0  heartbeat   │     │ terminal     │
│  xaus.com    │        │ 0x100  BTC         │ ──▶ │ price-       │
└──────┬───────┘        │  ...   ...         │     │ dashboard    │
       │                │ 0x105  DOGE        │     └──────────────┘
       ▼                │ 0x200  EUR/USD     │
┌──────────────┐        │  ...   ...         │     ┌──────────────┐
│ price-fetcher│  ───▶  │ 0x209  BTC/USD     │ ──▶ │ browser      │
└──────────────┘        └────────────────────┘     │ price-web    │
                                                   │ :8080        │
                                                   └──────────────┘
```

The CAN bus is the only link between the programs. Nothing is shared in memory, no
database, no files: exactly how a real vehicle or industrial device passes data between
control units.

---

## The three programs

| Folder | What it does |
|---|---|
| **price-fetcher** | Downloads prices from the internet and writes them to the CAN bus. The only program that touches the network. |
| **price-dashboard** | Reads the bus and prints the 6 crypto prices in the terminal, with up/down arrows. |
| **price-web** | Reads the bus, builds candlesticks, and serves a web page with live charts on port **8080**. |

### price-fetcher: three workers in parallel

| Worker | Every | Sends | Source |
|---|---|---|---|
| crypto | 5 s | `0x100`–`0x105` (BTC, ETH, SOL, XRP, ADA, DOGE) | Coinbase spot price |
| markets | 20 s | `0x200`–`0x209` (7 forex pairs, gold, silver, BTC) | Coinbase exchange rates + xaus.com metals |
| heartbeat | 1 s | `0x0F0` | — |

### price-web endpoints

| Address | Answer |
|---|---|
| `/` | the dashboard page (built into the program at compile time) |
| `/data` | the crypto price history + gateway status, as JSON |
| `/markets` | the 10 instruments with last price and change |
| `/candles?i=N` | all candles of instrument `N` as `[time, open, high, low, close]` |

The page has two tabs: **Crypto Monitor** (line charts, Chart.js) and **Markets /
TLB Infinitum Trading View** (candlesticks, TradingView Lightweight Charts, with an
Auto / Line / Candles switch and 1m / 5m / 15m / 1h timeframes).

---

## CAN message format

Every price frame carries **8 bytes: one `f64`, little-endian**.

```
ID     0x100          bytes  [ 8 bytes = f64 little-endian ]
                             e.g. 64608.21
```

The heartbeat on `0x0F0` is **3 bytes**:

```
byte 0   counter 0..255, +1 every second (proves the device is alive)
byte 1   how many of the 6 crypto prices worked in the last round
byte 2   how many of the 10 market prices worked in the last round
```

A receiver that only reads the first 2 bytes still works, which is why byte 2 was added
at the end rather than in the middle.

`price-web` marks the gateway **OFFLINE** if no heartbeat arrives for 3 seconds.

### ID map

| Range | Used | Meaning |
|---|---|---|
| `0x0F0` | 1 | gateway heartbeat |
| `0x100`–`0x1FF` | 6 | crypto prices |
| `0x200`–`0x2FF` | 10 | forex, metals, BTC |

Low IDs win arbitration on a real CAN bus, so the heartbeat is given the lowest ID.

---

## Running it on a normal Linux PC

Needs Rust (edition 2024) and a Linux kernel with SocketCAN. Not macOS or Windows —
SocketCAN is a Linux kernel feature. On Windows, WSL2 works.

**1. Create the virtual CAN bus** (once per boot):

```bash
sudo modprobe vcan                       # load the virtual CAN driver
sudo ip link add dev vcan0 type vcan     # create the bus
sudo ip link set up vcan0                # switch it on
```

**2. Start the fetcher** (one terminal):

```bash
cd price-fetcher && cargo run
```

**3. Start the web server** (another terminal):

```bash
cd price-web && cargo run
```

Then open <http://localhost:8080>.

For the terminal view instead, run `cd price-dashboard && cargo run`.

Watch the raw bus traffic with `candump vcan0` (from `can-utils`).

### Tests

```bash
cd price-web && cargo test
```

Covers the CAN ID mapping, URL parsing, candle building and the 24-hour cap.

---

## Running it as an embedded device

The real target is a Yocto-built Linux image where all of this starts automatically at
boot, with no login. See [meta-mokhles](https://github.com/adaliamokhles/meta-mokhles).

The Yocto recipes pin an exact commit of this repository (`SRCREV`), so changes here only
reach the device once that commit ID is updated in the layer.

---

## Branches

`main` is always a working version. Each feature is built on its own branch and merged
through a pull request once it runs:

| Branch | What it added |
|---|---|
| `feature/price-system` | the first version: 6 crypto prices, dashboard, web charts |
| `feature/heartbeat` | the gateway heartbeat and the online/offline badge |
| `feature/markets` | the Markets tab: 10 instruments and candlestick charts |

---

## Built with

Rust (`socketcan`, `ureq`, `serde_json`, `tiny_http`), Chart.js, and
[TradingView Lightweight Charts](https://www.tradingview.com/lightweight-charts/).

Price data from the public Coinbase and xaus.com APIs. For learning purposes, not for
trading decisions.

## License

MIT
