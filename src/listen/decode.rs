use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, CODEC_ID_NULL_AUDIO,
};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::Track;

use super::Result;

pub(super) fn audio_codec_params(track: &Track) -> Option<&AudioCodecParameters> {
    match track.codec_params.as_ref()? {
        CodecParameters::Audio(params) if params.codec != CODEC_ID_NULL_AUDIO => Some(params),
        _ => None,
    }
}

pub(super) fn make_audio_decoder(
    track: &Track,
    decoder_opts: &AudioDecoderOptions,
) -> Result<Box<dyn AudioDecoder>> {
    let params =
        audio_codec_params(track).ok_or_else(|| "no supported audio tracks".to_string())?;
    Ok(symphonia::default::get_codecs().make_audio_decoder(params, decoder_opts)?)
}
