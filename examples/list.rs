fn main() -> Result<(), chimeras::Error> {
    let devices = chimeras::devices()?;
    if devices.is_empty() {
        println!("no cameras found");
        return Ok(());
    }

    for device in &devices {
        println!("{}  ({})", device.name, device.id.0);
        let capabilities = chimeras::probe(device)?;
        for format in &capabilities.formats {
            println!(
                "  {}x{}  {:.0}-{:.0} fps  {:?}",
                format.resolution.width,
                format.resolution.height,
                format.framerate_range.min,
                format.framerate_range.max,
                format.pixel_format,
            );
        }
    }
    Ok(())
}
