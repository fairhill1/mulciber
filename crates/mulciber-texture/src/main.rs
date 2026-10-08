//! `mulciber-texture`: bakes materials' textures to BC7 KTX2 beside their sources.

fn main() -> Result<(), mulciber_texture::TextureError> {
    mulciber_texture::run(std::env::args().skip(1))
}
