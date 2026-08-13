#[cfg(feature = "experimental")]
mod experimental;
#[cfg(not(feature = "experimental"))]
mod stable;

#[cfg(feature = "experimental")]
pub(super) use experimental::run_listenmoe_stream;
#[cfg(not(feature = "experimental"))]
pub(super) use stable::run_listenmoe_stream;

use std::time::Duration;

use rodio::{source::Zero, MixerDeviceSink, Source};

use super::PlaybackVolume;

const VOLUME_REFRESH_INTERVAL: Duration = Duration::from_millis(10);

pub(super) fn attach_volume_mixer(
    device_sink: &MixerDeviceSink,
    volume: PlaybackVolume,
) -> rodio::mixer::Mixer {
    let config = device_sink.config();
    let (app_mixer, app_source) = rodio::mixer::mixer(config.channel_count(), config.sample_rate());

    app_mixer.add(Zero::new(config.channel_count(), config.sample_rate()));

    device_sink
        .mixer()
        .add(volume_controlled_source(app_source, volume));

    app_mixer
}

fn volume_controlled_source(
    source: rodio::mixer::MixerSource,
    volume: PlaybackVolume,
) -> impl Source + Send {
    source
        .amplify(volume.factor())
        .periodic_access(VOLUME_REFRESH_INTERVAL, move |source| {
            source.set_factor(volume.factor());
        })
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU16, NonZeroU32};

    use rodio::buffer::SamplesBuffer;

    use super::{volume_controlled_source, PlaybackVolume};

    #[test]
    fn active_source_tracks_shared_volume() {
        let channels = NonZeroU16::new(1).unwrap();
        // A 1 Hz test source makes the periodic callback run for every sample.
        let sample_rate = NonZeroU32::new(1).unwrap();
        let (mixer, source) = rodio::mixer::mixer(channels, sample_rate);
        mixer.add(SamplesBuffer::new(
            channels,
            sample_rate,
            vec![1.0, 1.0, 1.0],
        ));

        let volume = PlaybackVolume::default();
        let mut output = volume_controlled_source(source, volume.clone());
        assert_eq!(output.next(), Some(1.0));

        volume.set_percent(25);
        assert_eq!(output.next(), Some(0.25));

        volume.set_percent(0);
        assert_eq!(output.next(), Some(0.0));
    }
}
