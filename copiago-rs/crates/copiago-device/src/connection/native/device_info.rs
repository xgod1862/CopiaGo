use std::{path::Path, time::Duration};

use copiago_core::DeviceInfo;
use idevice::{
    IdeviceError, RsdService, lockdown::LockdownClient, rsd::RsdHandshake,
    tcp::handle::AdapterHandle,
};
use plist::Value;
use tokio::time::timeout;
use tracing::{Dispatch, instrument::WithSubscriber};

const NAME_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) async fn read(
    adapter: &mut AdapterHandle,
    rsd: &mut RsdHandshake,
    data_dir: &Path,
) -> DeviceInfo {
    let mut info = DeviceInfo::load(data_dir);
    let name = timeout(NAME_TIMEOUT, lockdown_name(adapter, rsd))
        .with_subscriber(Dispatch::none())
        .await;
    if let Ok(Ok(name)) = name
        && !name.is_empty()
    {
        info.name = name;
    }
    if let Some(product) = property(rsd, "ProductType") {
        info.model = model_name(product).to_owned();
    }
    if let Some(version) = property(rsd, "OSVersion").or_else(|| property(rsd, "ProductVersion")) {
        info.os_version = version.to_owned();
    }
    info
}

async fn lockdown_name(
    adapter: &mut AdapterHandle,
    rsd: &mut RsdHandshake,
) -> Result<String, IdeviceError> {
    let mut lockdown = match LockdownClient::connect_rsd(adapter, rsd).await {
        Ok(client) => client,
        Err(IdeviceError::ServiceNotFound) => {
            // The untrusted variant uses the same lockdown check-in.
            let port = rsd
                .services
                .get("com.apple.mobile.lockdown.remote.untrusted")
                .ok_or(IdeviceError::ServiceNotFound)?
                .port;
            let stream = adapter.connect(port).await?;
            <LockdownClient as RsdService>::from_stream(Box::new(stream)).await?
        }
        Err(error) => return Err(error),
    };
    let name = lockdown.get_value(Some("DeviceName"), None).await?;
    Ok(name.into_string().unwrap_or_default())
}

fn property<'a>(rsd: &'a RsdHandshake, key: &str) -> Option<&'a str> {
    rsd.properties
        .get(key)
        .and_then(Value::as_string)
        .filter(|value| !value.is_empty())
}

fn model_name(product: &str) -> &str {
    // iPhone models supported by the CoreDevice tunnel. Product identifiers:
    // https://raw.githubusercontent.com/devicekit/DeviceKit/master/Source/Device.generated.swift
    // Display strings follow CopiaGo's Python model_name.
    match product {
        "iPhone11,2" => "iPhone XS",
        "iPhone11,4" => "iPhone XS Max (China)",
        "iPhone11,6" => "iPhone XS Max",
        "iPhone11,8" => "iPhone XR",
        "iPhone12,1" => "iPhone 11",
        "iPhone12,3" => "iPhone 11 Pro",
        "iPhone12,5" => "iPhone 11 Pro Max",
        "iPhone12,8" => "iPhone SE (2nd gen)",
        "iPhone13,1" => "iPhone 12 mini",
        "iPhone13,2" => "iPhone 12",
        "iPhone13,3" => "iPhone 12 Pro",
        "iPhone13,4" => "iPhone 12 Pro Max",
        "iPhone14,2" => "iPhone 13 Pro",
        "iPhone14,3" => "iPhone 13 Pro Max",
        "iPhone14,4" => "iPhone 13 mini",
        "iPhone14,5" => "iPhone 13",
        "iPhone14,6" => "iPhone SE (3rd gen)",
        "iPhone14,7" => "iPhone 14",
        "iPhone14,8" => "iPhone 14 Plus",
        "iPhone15,2" => "iPhone 14 Pro",
        "iPhone15,3" => "iPhone 14 Pro Max",
        "iPhone15,4" => "iPhone 15",
        "iPhone15,5" => "iPhone 15 Plus",
        "iPhone16,1" => "iPhone 15 Pro",
        "iPhone16,2" => "iPhone 15 Pro Max",
        "iPhone17,1" => "iPhone 16 Pro",
        "iPhone17,2" => "iPhone 16 Pro Max",
        "iPhone17,3" => "iPhone 16",
        "iPhone17,4" => "iPhone 16 Plus",
        "iPhone17,5" => "iPhone 16e",
        "iPhone18,1" => "iPhone 17 Pro",
        "iPhone18,2" => "iPhone 17 Pro Max",
        "iPhone18,3" => "iPhone 17",
        "iPhone18,4" => "iPhone Air",
        "iPhone18,5" => "iPhone 17e",
        "iPhone19,2" => "iPhone 18 Pro",
        "iPhone19,3" => "iPhone 18 Pro Max (US)",
        "iPhone19,4" => "iPhone Duo",
        "iPhone19,7" => "iPhone 18 Pro Max",
        _ => product,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_product_type_and_keeps_unknown_identifiers() {
        for (product, model) in [
            ("iPhone11,4", "iPhone XS Max (China)"),
            ("iPhone12,8", "iPhone SE (2nd gen)"),
            ("iPhone16,2", "iPhone 15 Pro Max"),
            ("iPhone19,3", "iPhone 18 Pro Max (US)"),
            ("iPhone99,8", "iPhone99,8"),
            ("", ""),
        ] {
            assert_eq!(model_name(product), model);
        }
    }
}
