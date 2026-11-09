//! Версії модулів із відповіді `info/get_version` (лише читання).
use serde_json::Value;

/// Один модуль принтера (`ota`, `ams`, `esp32`…); відсутні поля лишаються порожніми.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleVersion {
    pub name: String,
    pub software: String,
    pub hardware: String,
    pub product: String,
}

/// Порожній вектор, якщо повідомлення не є відповіддю `get_version` з модулями.
pub fn module_versions(message: &Value) -> Vec<ModuleVersion> {
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    message["info"]["module"]
        .as_array()
        .map(|modules| {
            modules
                .iter()
                .filter_map(|m| {
                    let name = text(&m["name"]);
                    (!name.is_empty()).then(|| ModuleVersion {
                        name,
                        software: text(&m["sw_ver"]),
                        hardware: text(&m["hw_ver"]),
                        product: text(&m["product_name"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_modules_and_ignores_nameless_or_foreign_messages() {
        let message = json!({"info":{"command":"get_version","module":[
            {"name":"ota","sw_ver":"01.07.00.00","hw_ver":"","product_name":"Bambu Lab A1","sn":"PRIVATE"},
            {"name":"ams/0","sw_ver":"00.00.06.40","hw_ver":"AMS08"},
            {"sw_ver":"x"}
        ]}});
        let modules = module_versions(&message);
        assert_eq!(modules.len(), 2);
        assert_eq!(modules[0].software, "01.07.00.00");
        assert_eq!(modules[0].product, "Bambu Lab A1");
        assert_eq!(modules[1].hardware, "AMS08");
        assert!(module_versions(&json!({"print":{}})).is_empty());
    }
}
