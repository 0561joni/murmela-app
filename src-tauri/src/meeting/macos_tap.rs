//! System audio on macOS through a Core Audio process tap (macOS 14.4 or later).
//!
//! The tap mixes everything the Mac plays except Murmela's own sounds into a
//! stereo stream. It is wrapped in a private aggregate device that only this
//! process can see, which cpal then records like any input. Unlike cpal's
//! built-in loopback this does not depend on the output device: headsets and
//! AirPods, which also have a microphone, are tapped rather than recorded.
//! The first tap asks for "System Audio Recording" permission
//! (`NSAudioCaptureUsageDescription` in Info.plist).

use std::ffi::{c_void, CStr};
use std::ptr::NonNull;

use anyhow::{anyhow, Result};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, AudioHardwareCreateAggregateDevice,
    AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectID,
    AudioObjectPropertyAddress, CATapDescription, CATapMuteBehavior,
};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSMutableDictionary, NSNumber, NSString};

/// A process tap and the private aggregate device around it, destroyed on drop.
pub struct SystemAudioTap {
    tap_id: AudioObjectID,
    aggregate_id: AudioObjectID,
    /// Core Audio UID of the aggregate device, for cpal's `device_by_id`.
    pub device_uid: String,
}

// Both ids are plain integers owned by this value; Core Audio calls are thread-safe.
unsafe impl Send for SystemAudioTap {}

impl SystemAudioTap {
    pub fn create() -> Result<Self> {
        let excluded = own_process_object()
            .map(|id| vec![NSNumber::new_u32(id)])
            .unwrap_or_default();
        let excluded = NSArray::from_retained_slice(&excluded);
        let description = unsafe {
            CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &excluded,
            )
        };
        unsafe {
            description.setName(&NSString::from_str("Murmela call audio"));
            description.setPrivate(true);
            description.setMuteBehavior(CATapMuteBehavior::Unmuted);
        }
        let mut tap_id: AudioObjectID = 0;
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut tap_id) };
        if status != 0 || tap_id == 0 {
            return Err(anyhow!(
                "SYSTEM_AUDIO_UNAVAILABLE: macOS did not allow recording the call's sound (Core Audio error {status}). Recording calls needs macOS 14.4 or later."
            ));
        }
        let tap_uid = unsafe { description.UUID().UUIDString() };
        let device_uid = format!("com.murmela.call-audio.{tap_uid}");

        let sub_tap = NSMutableDictionary::<NSString, AnyObject>::new();
        insert(&sub_tap, kAudioSubTapUIDKey, &tap_uid);
        insert(
            &sub_tap,
            kAudioSubTapDriftCompensationKey,
            &NSNumber::new_bool(true),
        );
        let taps = NSArray::from_retained_slice(&[sub_tap]);

        let properties = NSMutableDictionary::<NSString, AnyObject>::new();
        insert(
            &properties,
            kAudioAggregateDeviceNameKey,
            &NSString::from_str("Murmela call audio"),
        );
        insert(
            &properties,
            kAudioAggregateDeviceUIDKey,
            &NSString::from_str(&device_uid),
        );
        insert(
            &properties,
            kAudioAggregateDeviceIsPrivateKey,
            &NSNumber::new_bool(true),
        );
        insert(
            &properties,
            kAudioAggregateDeviceTapAutoStartKey,
            &NSNumber::new_bool(true),
        );
        insert(&properties, kAudioAggregateDeviceTapListKey, &taps);

        let mut aggregate_id: AudioObjectID = 0;
        // NSDictionary is toll-free bridged to CFDictionary.
        let dictionary = unsafe { &*(Retained::as_ptr(&properties) as *const CFDictionary) };
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(dictionary, NonNull::from(&mut aggregate_id))
        };
        if status != 0 || aggregate_id == 0 {
            unsafe { AudioHardwareDestroyProcessTap(tap_id) };
            return Err(anyhow!(
                "SYSTEM_AUDIO_UNAVAILABLE: macOS could not prepare the call's sound for recording (Core Audio error {status})."
            ));
        }
        Ok(Self {
            tap_id,
            aggregate_id,
            device_uid,
        })
    }
}

impl Drop for SystemAudioTap {
    fn drop(&mut self) {
        unsafe {
            AudioHardwareDestroyAggregateDevice(self.aggregate_id);
            AudioHardwareDestroyProcessTap(self.tap_id);
        }
    }
}

fn insert(dictionary: &NSMutableDictionary<NSString, AnyObject>, key: &CStr, value: &AnyObject) {
    let key = NSString::from_str(key.to_str().unwrap_or_default());
    unsafe { dictionary.setObject_forKey(value, objc2::runtime::ProtocolObject::from_ref(&*key)) };
}

/// Murmela's own Core Audio process object, so its sounds stay out of the recording.
fn own_process_object() -> Option<AudioObjectID> {
    let pid = std::process::id() as i32;
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut object: AudioObjectID = 0;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            std::mem::size_of::<i32>() as u32,
            &pid as *const i32 as *const c_void,
            NonNull::from(&mut size),
            NonNull::from(&mut object).cast(),
        )
    };
    (status == 0 && object != 0).then_some(object)
}
