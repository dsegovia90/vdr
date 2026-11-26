//! Shared UI components.

mod bar;
mod button;
mod call_screen;
mod colour;
mod connect;
mod device_menu;
mod icons;
mod keys;
mod layout;
mod microphone;
mod participant;
mod recent;
mod settings;
mod share;
mod signal;
mod stats;
mod subroom;
mod tile;
mod tile_menu;

pub use bar::ControlBar;
pub use button::{Button, Size, Tone};
pub use device_menu::{DeviceMenu, DropdownOption, MenuSection, use_open_menu_provider};
pub use icons::{CameraIcon, ChartIcon, CloseIcon, FlaskIcon, GearIcon, InfoIcon, KeyboardIcon, LeaveIcon, MicIcon, PersonIcon, PinIcon, ScreenIcon, SpeakerIcon, TopologyIcon, UpdateIcon};
pub use keys::{KeyFocus, SettingsPanel};
pub use call_screen::CallScreen;
pub use connect::ConnectRow;
pub use microphone::MicControl;
pub use participant::ParticipantVideo;
pub use recent::RecentRoomList;
pub use settings::SettingsModal;
use settings::{UpdateBanner, Updates, use_updates_provider};
pub use signal::SignalBars;
pub use stats::StatsRow;
pub use share::{ShareControl, ShareTile};
pub use subroom::{CrosstalkLevel, Invites, Names, OwnShare, Rooms, SubroomMenu, Through, use_crosstalk_provider, use_rooms_provider};
pub use tile::ParticipantTile;
