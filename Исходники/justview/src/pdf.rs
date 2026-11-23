//! Рендер страниц PDF встроенным движком Windows (Windows.Data.Pdf).

use std::path::{Path, PathBuf};

use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

use crate::winutil::Rgba;

fn render(bytes: &[u8], page: u32, width: u32) -> windows::core::Result<(u32, Vec<u8>)> {
    // Документ читается из памяти: не зависит от прав StorageFile и особенностей путей.
    let src = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&src.GetOutputStreamAt(0)?)?;
    writer.WriteBytes(bytes)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    let _ = writer.DetachStream();
    src.Seek(0)?;
    let doc = PdfDocument::LoadFromStreamAsync(&src)?.join()?;

    let count = doc.PageCount()?;
    let p = doc.GetPage(page.min(count.saturating_sub(1)))?;
    let out = InMemoryRandomAccessStream::new()?;
    let opts = PdfPageRenderOptions::new()?;
    opts.SetDestinationWidth(width)?;
    p.RenderWithOptionsToStreamAsync(&out, &opts)?.join()?;
    let size = out.Size()? as u32;
    let reader = DataReader::CreateDataReader(&out.GetInputStreamAt(0)?)?;
    reader.LoadAsync(size)?.join()?;
    let mut png = vec![0u8; size as usize];
    reader.ReadBytes(&mut png)?;
    Ok((count, png))
}

/// Возвращает (число страниц, изображение страницы). Выполняется в MTA-потоке.
pub fn render_page(path: &Path, page: u32, width: u32) -> Result<(u32, Rgba), String> {
    let path: PathBuf = path.to_path_buf();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let (count, png) = render(&bytes, page, width).map_err(|e| e.message().to_string())?;
        let img = image::load_from_memory(&png).map_err(|e| e.to_string())?.into_rgba8();
        let (w, h) = img.dimensions();
        Ok((count, Rgba { w, h, px: img.into_raw() }))
    })
    .join()
    .unwrap_or_else(|_| Err(tr!("сбой рендера PDF", "PDF render failed", "збій рендерингу PDF").into()))
}

pub fn open(path: &Path, width: u32) -> Result<(u32, Rgba), String> {
    render_page(path, 0, width)
}
