use std::convert::TryFrom;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use uucore::exit;

mod app;
mod constants;
mod parse;
mod split;
use app::*;

fn rbuf_n_bytes(input: &mut impl std::io::BufRead, n: usize) -> std::io::Result<()> {
    if n == 0 {
        return Ok(())
    }
    let mut readbuf = [0u8; constants::BUF_SIZE];
    let mut i = 0usize;

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    loop {
        let read = input.read(&mut readbuf)?;
        if read == 0 {
            // might be unexpected if
            // we haven't read `n` bytes
            // but this mirrors GNU's behavior
            return Ok(());
        }
        stdout.write_all(&readbuf[..read.min(n - i)])?;
        i += read.min(n - i);
        if i == n {
            return Ok(());
        }
    }
}

fn rbuf_n_lines(input: &mut impl std::io::BufRead, n: usize, zero: bool) -> std::io::Result<()> {
    if n == 0 {
        return Ok(());
    }
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let mut lines = 0usize;
    split::walk_lines(input, zero, |e| match e {
        split::Event::Data(dat) => {
            stdout.write_all(dat)?;
            Ok(true)
        }
        split::Event::Line => {
            lines += 1;
            if lines == n {
                Ok(false)
            } else {
                Ok(true)
            }
        }
    })
}

fn rbuf_but_last_n_bytes(input: &mut impl std::io::BufRead, n: usize) -> std::io::Result<()> {
    if n == 0 {
        //prints everything
        return rbuf_n_bytes(input, usize::MAX);
    }
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    let mut ringbuf = vec![0u8; n];

    // first we fill the ring buffer
    if let Err(e) = input.read_exact(&mut ringbuf) {
        if e.kind() == ErrorKind::UnexpectedEof {
            return Ok(());
        } else {
            return Err(e);
        }
    }
    let mut buffer = [0u8; constants::BUF_SIZE];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        } else if read >= n {
            stdout.write_all(&ringbuf)?;
            stdout.write_all(&buffer[..read - n])?;
            for i in 0..n {
                ringbuf[i] = buffer[read - n + i];
            }
        } else {
            stdout.write_all(&ringbuf[..read])?;
            for i in 0..n - read {
                ringbuf[i] = ringbuf[read + i];
            }
            ringbuf[n - read..].copy_from_slice(&buffer[..read]);
        }
    }
}

fn rbuf_but_last_n_lines(
    input: &mut impl std::io::BufRead,
    n: usize,
    zero: bool,
) -> std::io::Result<()> {
    if n == 0 {
        //prints everything
        return rbuf_n_bytes(input, usize::MAX);
    }
    let mut ringbuf = vec![Vec::new(); n];
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let mut line = Vec::new();
    let mut lines = 0usize;
    split::walk_lines(input, zero, |e| match e {
        split::Event::Data(dat) => {
            line.extend_from_slice(dat);
            Ok(true)
        }
        split::Event::Line => {
            if lines < n {
                ringbuf[lines] = std::mem::replace(&mut line, Vec::new());
                lines += 1;
            } else {
                stdout.write_all(&ringbuf[0])?;
                ringbuf.rotate_left(1);
                ringbuf[n - 1] = std::mem::replace(&mut line, Vec::new());
            }
            Ok(true)
        }
    })
}

fn head_backwards_file(input: &mut std::fs::File, options: &HeadOptions) -> std::io::Result<()> {
    assert!(options.all_but_last);
    let size = input.seek(SeekFrom::End(0))?;
    let size = usize::try_from(size).unwrap();
    match options.mode {
        Modes::Bytes(n) => {
            if n >= size {
                return Ok(());
            } else {
                input.seek(SeekFrom::Start(0))?;
                rbuf_n_bytes(
                    &mut std::io::BufReader::with_capacity(constants::BUF_SIZE, input),
                    size - n,
                )?;
            }
        }
        Modes::Lines(n) => {
            let mut buffer = [0u8; constants::BUF_SIZE];
            let buffer = &mut buffer[..constants::BUF_SIZE.min(size)];
            let mut i = 0usize;
            let mut lines = 0usize;

            let found = 'o: loop {
                // the casts here are ok, `buffer.len()` should never be above a few k
                input.seek(SeekFrom::Current(
                    -((buffer.len() as i64).min((size - i) as i64)),
                ))?;
                input.read_exact(buffer)?;
                for byte in buffer.iter().rev() {
                    match byte {
                        b'\n' if !options.zeroed => {
                            lines += 1;
                        }
                        0u8 if options.zeroed => {
                            lines += 1;
                        }
                        _ => {}
                    }
                    // if it were just `n`,
                    if lines == n + 1 {
                        break 'o i;
                    }
                    i += 1;
                }
                if size - i == 0 {
                    return Ok(());
                }
            };
            input.seek(SeekFrom::Start(0))?;
            rbuf_n_bytes(
                &mut std::io::BufReader::with_capacity(constants::BUF_SIZE, input),
                size - found,
            )?;
        }
    }
    Ok(())
}

fn head_file(input: &mut std::fs::File, options: &HeadOptions) -> std::io::Result<()> {
    if options.all_but_last {
        head_backwards_file(input, options)
    } else {
        match options.mode {
            Modes::Bytes(n) => rbuf_n_bytes(
                &mut std::io::BufReader::with_capacity(constants::BUF_SIZE, input),
                n,
            ),
            Modes::Lines(n) => rbuf_n_lines(
                &mut std::io::BufReader::with_capacity(constants::BUF_SIZE, input),
                n,
                options.zeroed,
            ),
        }
    }
}

fn uu_head(options: &HeadOptions) {
    let mut first = true;
    for fname in &options.files {
        let res = match fname.as_str() {
            "-" => {
                if options.verbose {
                    if !first {
                        println!();
                    }
                    println!("==> standard input <==")
                }
                let stdin = std::io::stdin();
                let mut stdin = stdin.lock();
                match options.mode {
                    Modes::Bytes(n) => {
                        if options.all_but_last {
                            rbuf_but_last_n_bytes(&mut stdin, n)
                        } else {
                            rbuf_n_bytes(&mut stdin, n)
                        }
                    }
                    Modes::Lines(n) => {
                        if options.all_but_last {
                            rbuf_but_last_n_lines(&mut stdin, n, options.zeroed)
                        } else {
                            rbuf_n_lines(&mut stdin, n, options.zeroed)
                        }
                    }
                }
            }
            name => {
                let mut file = match std::fs::File::open(name) {
                    Ok(f) => f,
                    Err(err) => match err.kind() {
                        ErrorKind::NotFound => {
                            eprintln!(
                                "head: cannot open '{}' for reading: No such file or directory",
                                name
                            );
                            exit!(constants::EXIT_FAILURE);
                        }
                        ErrorKind::PermissionDenied => {
                            eprintln!(
                                "head: cannot open '{}' for reading: Permission denied",
                                name
                            );
                            exit!(constants::EXIT_FAILURE);
                        }
                        _ => {
                            eprintln!("head: cannot open '{}' for reading: {}", name, err);
                            exit!(constants::EXIT_FAILURE);
                        }
                    },
                };
                if (options.files.len() > 1 && !options.quiet) || options.verbose {
                    println!("==> {} <==", name)
                }
                head_file(&mut file, options)
            }
        };
        if res.is_err() {
            if fname.as_str() == "-" {
                eprintln!("head: error reading standard input: Input/output error");
            } else {
                eprintln!("head: error reading {}: Input/output error", fname);
            }
            exit!(constants::EXIT_FAILURE);
        }
        first = false;
    }
}

pub fn uumain(args: impl uucore::Args) -> i32 {
    let args = match HeadOptions::get_from(args) {
        Ok(o) => o,
        Err(s) => {
            eprintln!("head: {}", s);
            exit!(constants::EXIT_FAILURE);
        }
    };
    uu_head(&args);

    constants::EXIT_SUCCESS
}
