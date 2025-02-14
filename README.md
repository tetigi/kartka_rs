# Kartka-rs

Quick and dirty indexing of your mail and PDFs so you can go paperless.

### What is it?

1. Get letters.
2. Scan them as PDFs.
3. `kartka scan`
4. Kartka OCRs the text from the PDF, uploads the contents to an index, then uploads the PDF to Dropbox.
5. Search all of your prior mail via free-text search via `kartka search`.
6. ???
7. Profit!

### Why is it?

I hate paper mail, and I hate trying to find paper mail after I've stored it somewhere.

### How does it work?

- Tesseract is used to OCR the images.
- The 'index' is just a flat folder containing text files with the contents of each PDF.
- The 'search' is just `ripgrep`.
- That's it.

The index is stored locally on your device of choice - I run this on my laptop. It could be stored on some remote server but I only have one computer so I haven't added that yet.

To rehydrate the index from old letters in Dropbox, run `kartka hydrate`.

### How do I install it?

Everything you need is in the `flake.nix`.

Kartka must be configured by putting a `kartka.toml` at `~/.config/kartka.toml`.

It must contain the following values:

```toml
scan_dir = "/Users/my.user/Downloads/kartka"
index_dir = "/Users/my.userDocuments/personal/kartka"
```

The `scan_dir` determines where `kartka scan` looks to pick up your newly scanned letters.

The `index_dir` is where the index will live on your device.
