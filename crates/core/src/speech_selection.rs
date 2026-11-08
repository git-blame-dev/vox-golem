//! Portable policy over SDK-normalized observed voices. Locale discovery stays native.
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceDescriptor {
    pub name: String,
    pub language: String,
    pub script: String,
    pub region: String,
    pub network_required: bool,
    pub installed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceSelection {
    pub language: String,
    pub script: String,
    pub region: String,
    pub network_allowed: bool,
    pub explicit_voice: String,
    pub default_voice: String,
}
impl VoiceSelection {
    pub fn eligible(&self, voice: &VoiceDescriptor) -> bool {
        voice.installed
            && (self.network_allowed || !voice.network_required)
            && (self.explicit_voice.is_empty() || self.explicit_voice == voice.name)
            && self.language == voice.language
            && (self.script.is_empty() || voice.script.is_empty() || self.script == voice.script)
    }
    /// Installed local voices precede network routes, then region/default/name.
    /// Explicit unavailable choices return None; never silently select another ID.
    pub fn select(&self, voices: &[VoiceDescriptor]) -> Option<usize> {
        voices
            .iter()
            .enumerate()
            .filter(|(_, v)| self.eligible(v))
            .min_by_key(|(_, v)| {
                (
                    v.network_required,
                    v.region != self.region,
                    v.name != self.default_voice,
                    v.name.as_str(),
                )
            })
            .map(|(index, _)| index)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn voice(name: &str, region: &str, network: bool) -> VoiceDescriptor {
        VoiceDescriptor {
            name: name.into(),
            language: "en".into(),
            script: "".into(),
            region: region.into(),
            network_required: network,
            installed: true,
        }
    }
    #[test]
    fn observed_installed_local_then_region_then_default_and_explicit_rejection() {
        let mut request = VoiceSelection {
            language: "en".into(),
            script: "".into(),
            region: "AU".into(),
            network_allowed: true,
            explicit_voice: "".into(),
            default_voice: "default".into(),
        };
        let mut voices = vec![
            voice("network", "AU", true),
            voice("local", "US", false),
            voice("default", "AU", false),
        ];
        assert_eq!(request.select(&voices), Some(2));
        voices[2].installed = false;
        assert_eq!(request.select(&voices), Some(1));
        request.explicit_voice = "network".into();
        assert_eq!(request.select(&voices), Some(0));
        request.network_allowed = false;
        assert_eq!(request.select(&voices), None);
        request.explicit_voice = "missing".into();
        assert_eq!(request.select(&voices), None);
        request.explicit_voice.clear();
        request.script = "Latn".into();
        voices[1].script = "Cyrl".into();
        assert_eq!(request.select(&voices), None);
    }
}
