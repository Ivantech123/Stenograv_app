//! Скачивание больших файлов с докачкой: обрыв на 700 МБ из 874 не начинает всё заново.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::Cancelled;

pub fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("Stenograf/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .timeout(None::<Duration>)
        .build()?)
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// Скачивает `url` в `dest`. `on_progress(получено, всего)` зовётся не чаще 5 раз в секунду.
pub fn fetch(
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, Option<u64>),
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = part_path(dest);
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

    let client = client()?;
    let mut request = client.get(url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = request.send().with_context(|| format!("Нет связи с {}", host(url)))?;
    let status = response.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && have > 0 {
        // Кусок уже целиком скачан в прошлый раз.
        std::fs::rename(&part, dest)?;
        return Ok(());
    }
    if !status.is_success() {
        bail!("Сервер {} ответил {}", host(url), status);
    }
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    if !resumed {
        have = 0;
    }
    let total = response.content_length().map(|len| len + have);

    let mut file: File = if resumed {
        OpenOptions::new().append(true).open(&part)?
    } else {
        File::create(&part)?
    };

    let mut buf = vec![0u8; 256 * 1024];
    let mut last = Instant::now() - Duration::from_secs(1);
    on_progress(have, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            file.flush()?;
            return Err(Cancelled.into());
        }
        let n = response.read(&mut buf).context("Связь оборвалась, скачанное сохранено — повторите")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        have += n as u64;
        if last.elapsed() >= Duration::from_millis(200) {
            last = Instant::now();
            on_progress(have, total);
        }
    }
    file.flush()?;
    drop(file);
    if let Some(total) = total {
        if have != total {
            bail!("Файл скачан не полностью ({have} из {total} байт). Повторите — докачается.");
        }
    }
    on_progress(have, total);
    std::fs::rename(&part, dest).with_context(|| format!("Не удалось сохранить {}", dest.display()))?;
    Ok(())
}

fn host(url: &str) -> &str {
    url.split("://").nth(1).and_then(|rest| rest.split('/').next()).unwrap_or(url)
}
