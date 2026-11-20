//! Бэктест стратегии пересечения скользящих средних на реальных OHLCV-свечах из CSV.
//! Только стандартная библиотека.
//!
//! Запуск: cargo run --release [-- fast slow]
//! Пример: cargo run --release -- 3 10
//!
//! Формат CSV: строка заголовков, время в первой колонке, далее колонки
//! open, high, low, close (в любом порядке, регистр не важен) и необязательная volume.


use std::env;
use std::error::Error;
use std::fs;
use std::process;


/// Путь к файлу со свечами. Относительный путь считается от папки, из которой
/// запускается `cargo run` (обычно корень проекта, рядом с Cargo.toml).
/// Абсолютный путь в Windows можно записать так: r"C:\data\gold.csv".
const CSV_PATH: &str = "src/XAUUSD-5m-2026-11-17.csv";


type Result<T> = std::result::Result<T, Box<dyn Error>>;


/// Одна свеча.
struct Bar
{
  time: String,
  open: f64,
  high: f64,
  low: f64,
  close: f64,
  volume: f64,
}

#[derive(Debug, Clone, Copy)]
enum Signal
{
  Buy,
  Sell,
  Hold,
}

/// Читает свечи из CSV. Колонки ищутся по названию в заголовке, а не по позиции.
fn load_csv(path: &str) -> Result<Vec<Bar>>
{
  let text = fs::read_to_string(path).map_err(|e| format!("cannot read '{path}': {e}"))?;
  let mut lines = text.lines();

  let header: Vec<String> = lines
    .next()
    .ok_or("file is empty")?
    .split(',')
    .map(|h| h.trim().to_lowercase())
    .collect();

  let col = |name: &str| header.iter().position(|h| h == name);
  let need = |name: &str| col(name).ok_or_else(|| format!("column '{name}' not found"));

  let (o, h, l, c) = (need("open")?, need("high")?, need("low")?, need("close")?);
  let v = col("volume"); // необязательная: объём есть не у всех источников

  let mut bars = Vec::new();

  for (n, line) in lines.enumerate()
  {
    let line = line.trim();
    if line.is_empty() { continue; }

    let fields: Vec<&str> = line.split(',').map(str::trim).collect();
    let line_no = n + 2; // +1 за заголовок, +1 потому что строки нумеруются с 1

    let num = |i: usize| -> Result<f64>
    {
      let raw = fields.get(i).ok_or(format!("line {line_no}: too few columns"))?;
      raw.parse().map_err(|_| format!("line {line_no}: '{raw}' is not a number").into())
    };

    bars.push(Bar
    {
      time: fields[0].to_string(),
      open: num(o)?,
      high: num(h)?,
      low: num(l)?,
      close: num(c)?,
      volume: match v
      {
        Some(i) => num(i)?,
        None => 0.0,
      },
    });
  }

  // Некоторые API отдают сначала самую свежую свечу. ISO-время корректно
  // сравнивается как строка, так что приводим ряд к порядку «старые → новые».
  if bars.len() > 1 && bars[0].time > bars[bars.len() - 1].time
  	{ bars.reverse(); }

   Ok(bars)
}

/// Простая скользящая средняя по `window` значениям, заканчивая индексом `i` включительно.
fn sma(values: &[f64], i: usize, window: usize) -> f64 {
  values[i + 1 - window..=i].iter().sum::<f64>() / window as f64
}

/// Быстрая SMA выше медленной → восходящий тренд → хотим быть в позиции.
fn strategy(closes: &[f64], i: usize, fast: usize, slow: usize) -> Signal
{
  let (f, s) = (sma(closes, i, fast), sma(closes, i, slow));
  if f > s { Signal::Buy }
  else if f < s { Signal::Sell }
  else { Signal::Hold }
}

fn run() -> Result<()>
{
  const FEE: f64 = 0.0002; // 0.02% за сделку ≈ спред + комиссия на золоте
  const START_CASH: f64 = 10_000.0;


  let args: Vec<String> = env::args().collect();
  let fast: usize = args.get(1).map_or(Ok(5), |s| s.parse())?;
  let slow: usize = args.get(2).map_or(Ok(20), |s| s.parse())?;


  if fast == 0 || fast >= slow
   	{ return Err("windows must satisfy 0 < fast < slow".into()); }


  let bars = load_csv(CSV_PATH)?;
  if bars.len() <= slow {
    return Err(format!("need more than {slow} bars for SMA({slow}), file has {}", bars.len()).into());
  }

  let first = &bars[0];
  let last = &bars[bars.len() - 1];

  let lo = bars.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
  let hi = bars.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max);

  let vol: f64 = bars.iter().map(|b| b.volume).sum();

  println!("Loaded {} bars: {} → {}", bars.len(), first.time, last.time);
  println!("Range {lo:.2} – {hi:.2}, total volume {vol:.0}");
  println!("Strategy: SMA({fast}) / SMA({slow}), fee {:.2}%\n", FEE * 100.0);

  let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();

  let mut cash = START_CASH;
  let mut units = 0.0;
  let mut trades = 0;
  let mut peak = START_CASH;
  let mut max_drawdown = 0.0_f64;

  // Сигнал считаем по ЗАКРЫТИЮ бара i, ордер исполняем по ОТКРЫТИЮ бара i+1:
  // это самый ранний момент, когда реальный робот мог бы отреагировать на сигнал.
  for i in (slow - 1)..bars.len() - 1
  {
    let next = &bars[i + 1];
    let in_market = units > 0.0;

    match strategy(&closes, i, fast, slow)
    {
      Signal::Buy if !in_market =>
      {
        units = cash * (1.0 - FEE) / next.open;
        cash = 0.0;
        trades += 1;
        println!("{}  BUY   @ {:.2}", next.time, next.open);
      }
      Signal::Sell if in_market =>
      {
        cash = units * next.open * (1.0 - FEE);
        units = 0.0;
        trades += 1;
        println!("{}  SELL  @ {:.2}", next.time, next.open);
      }
      _ => {} // уже в нужной позиции или сигнала нет
    }

    // Переоцениваем портфель по цене закрытия бара.
    let equity = cash + units * next.close;
    peak = peak.max(equity);
    max_drawdown = max_drawdown.max((peak - equity) / peak);
  }

  let equity = cash + units * last.close; // открытая позиция оценивается по последнему закрытию
  let strat_ret = (equity / START_CASH - 1.0) * 100.0;

  // «Купил и держи» входит в тот же момент, когда стратегия впервые может торговать.
  let hold_ret = (last.close / bars[slow].open - 1.0) * 100.0;

  println!();
  println!("Trades:          {trades}");
  println!("Open position:   {}", if units > 0.0 { "yes" } else { "no" });
  println!("Strategy return: {strat_ret:>7.3}%");
  println!("Buy & hold:      {hold_ret:>7.3}%");
  println!("Max drawdown:    {:>7.3}%", max_drawdown * 100.0);

  Ok(())
}

fn main()
{
  if let Err(e) = run()
  {
    eprintln!("error: {e}");
    process::exit(1);
  }
}
