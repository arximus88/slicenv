# Тестові сертифікати (не від Bambu)

Синтетичний ланцюжок для тестів `tls::PrinterVerifier`. Ключі тут — лише для тестів.

| Файл | Що це |
|---|---|
| `ca.pem` | тестовий кореневий CA (замість BBL CA) |
| `leaf.pem` + `leaf.key.pem` | X.509 **v1**, `CN=TESTSERIAL0001`, підписаний `ca.pem` — як листовий сертифікат справжнього A1 |
| `rogue.pem`, `rogue_leaf.pem` | «чужий» CA і v1-лист із тим самим CN |
| `int.pem`, `int_leaf.pem` | проміжний ECDSA CA під `ca.pem` і v3-лист `CN=TESTSERIAL0002` під ним |

OpenSSL 3.6 завжди видає v3, тому v1-листи зроблено з v3: `make_v1.py` прибирає
поля `version` і `extensions` з TBS і переподписує ключем CA (`openssl dgst -sha256 -sign`).
Термін дії — 100 років.
