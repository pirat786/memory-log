# Публикация Memory Log

GitHub Releases раздаёт установщик и файл обновлений. GitHub Pages может разместить страницу приложения, политику конфиденциальности и условия для экрана согласия Google. Эти два сервиса работают без отдельного сервера. Собственный домен для GitHub не нужен.

## Сайт и настройки Google

1. После попадания файлов в `main` откройте **Settings → Pages → Build and deployment** в репозитории `pirat786/memory-log`. Выберите **Deploy from a branch**, ветку `main` и папку `/docs`. Дождитесь публикации сайта.
2. Для подтверждения владения в рамках проверки OAuth-бренда Google указывает ресурс типа **Домен** в Search Console и проверку через DNS TXT от имени владельца проекта Google Cloud. У стандартного адреса `pirat786.github.io` DNS управляет GitHub. Для этого шага подключите к GitHub Pages домен, DNS которого вы можете редактировать. Если нужен только пробный вход с обычного аккаунта, сначала проверьте его в режиме **In production** с текущим доступом `drive.appdata`.
3. В Google Auth Platform откройте **Branding**. Добавьте адрес сайта в **Authorized domains** и заполните поля. Пример ниже относится к стандартному адресу GitHub Pages. Если подключили собственный домен для проверки бренда, подставьте опубликованные адреса на нём.

   | Поле | Адрес |
   | --- | --- |
   | Application home page | `https://pirat786.github.io/memory-log/` |
   | Application privacy policy link | `https://pirat786.github.io/memory-log/privacy.html` |
   | Application terms of service link | `https://pirat786.github.io/memory-log/terms.html` |

4. Убедитесь, что включён Google Drive API, тип клиента **Desktop app**, в **Data Access** указан только `https://www.googleapis.com/auth/drive.appdata`. В **Audience** выберите **External → Publish app**. Если в **Branding** появится **Verify branding**, отправьте бренд на проверку и после одобрения нажмите **Publish branding**.

`drive.appdata` относится к нечувствительным областям доступа по классификации Google. Проверка бренда и смена режима с **Testing** на **In production** выполняются отдельно. Проверьте вход с обычного аккаунта, которого нет в тестовом списке. Каждый пользователь входит в свой Google-аккаунт на своих ПК через приложение.

[Настройки бренда Google](https://support.google.com/cloud/answer/15549049) · [Настройки аудитории](https://support.google.com/cloud/answer/15549945) · [Подтверждение домена для OAuth](https://support.google.com/cloud/answer/13804266)

## Подписанный установщик и обновления

Один раз создайте ключ Tauri. Например, после установки CLI выполните `tauri signer generate --write-keys "$env:USERPROFILE\.memory-log-updater.key"`. Храните резервную копию закрытого ключа и его пароля в защищённом месте. Для всех следующих версий используйте тот же ключ. Установленный Memory Log проверит подпись новых выпусков встроенным открытым ключом.

В репозитории откройте **Settings → Secrets and variables → Actions** и задайте параметры:

| Имя | Раздел | Значение |
| --- | --- | --- |
| `TAURI_SIGNING_PUBLIC_KEY` | Variables | Содержимое файла открытого ключа `.pub` |
| `MEMORY_LOG_GOOGLE_CLIENT_ID` | Variables | Client ID настольного клиента, предназначенного для выпуска |
| `TAURI_SIGNING_PRIVATE_KEY` | Secrets | Содержимое файла закрытого ключа |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Secrets | Пароль ключа, если он задан |
| `MEMORY_LOG_GOOGLE_CLIENT_SECRET` | Secrets | `installed.client_secret` OAuth-клиента для выпуска |

Tauri подписывает установщик закрытым ключом. В программу попадают открытый ключ и параметры настольного OAuth-клиента. Настольный `client_secret` можно извлечь из установщика, поэтому для выпуска используйте клиент типа **Desktop app**, который готовы распространять. Не добавляйте JSON клиента или закрытый ключ Tauri в Git. Пользовательские токены выдаются только после согласия конкретного пользователя.

Для выпуска установите новую версию в `Cargo.toml` и `app/tauri.conf.json`, обновите `Cargo.lock` и отправьте совпадающий тег `vX.Y.Z`. Workflow соберёт Windows x64 NSIS, подпишет установщик и создаст черновик релиза с `latest.json`. Проверьте черновик перед публикацией и пометьте опубликованный стабильный выпуск как **Latest**.

Первый установщик с обновлениями нужно скачать с GitHub Releases. Дальше Memory Log проверяет наличие новой версии при открытии и предлагает установить её в настройках. Подпись Tauri проверяет подлинность обновления внутри приложения. Windows отдельно проверяет подпись издателя установщика.

[Обновления Tauri](https://v2.tauri.app/plugin/updater/) · [Публикация GitHub Pages](https://docs.github.com/en/pages/getting-started-with-github-pages/configuring-a-publishing-source-for-your-github-pages-site)
