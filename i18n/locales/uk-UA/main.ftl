### TERMINOLOGY USED
###
### map          → мапа (not карта)               
### folder       → тека (not папка)
### package      → пакунок (not пакет)
### settings     → налаштування — the values a user configures.
### parameters   → параметри — only the declarations in a package manifest
###                (manifest-param-*).
### delete       → Видалити — destroys the thing.
### remove       → Вилучити — takes it out of a set; the thing may survive.
### uninstall    → Деінсталювати
### debugging    → зневадження (not налагодження)
### handle       → дескриптор
### control characters/bytes → символи керування / байти керування
###                (not керівні символи, which means "governing").
### by default   → типово (not за замовчуванням, a calque of по умолчанию).
###
### PUNCTUATION
###
### Apostrophe: one character throughout (U+0027), as in ім'я, з'єднання.
### Quotation marks: «» for quoted names and UI labels.
### Ellipsis: … (U+2026), never three periods.
###
### PLURALS
###
### Ukrainian has four CLDR categories, and [one] fires on 1, 21, 31, 101 —
### not only on 1. Three rules are:
###
### 1. Never hardcode the digit inside a branch. Write { $count } пакунок,
###    not 1 пакунок, or 21 renders as "1".
### 2. When the numeral is printed, all four branches are needed, and the verb
###    agrees too: [one] takes a singular verb (21 пакунок потребує),
###    [few] and [many] take plural.
### 3. *[other] fires only on fractional values, which take the genitive
###    singular: 1,5 дня, 1,5 пакунка. It is not a synonym for [many].
###
### Where a string is a badge or a stat rather than a sentence, prefer
### label-first (невдалих: { $count }) over a selector — the number then sits
### outside the agreement chain and no branch is needed. See mapper-sync-failed,
### mapper-secret-count-warning, mapper-route-preview-stats.
###
### If the English source shows a count that Ukrainian needs but the source
### omits, the count may be added on this side (see package-removal-required)
### but flag it, since the two locales then differ in what they display.
###
### REGISTER
###
### Address the user with ви, lowercase.
### Checkbox and toggle labels take the infinitive: Показувати, Приховувати,
### Увімкнути — not the imperative.
### Confirmations use Справді видалити…? — not the calqued
### Ви впевнені, що хочете видалити…?
### Avoid the copula є in affirmative statements (Поле обов'язкове, not
### Поле є обов'язковим). Negations keep не є, which is normal Ukrainian.



# Locale names
locale-system = Системна мова
locale-english = Англійська (США)
locale-polish = Польська
locale-ukrainian = Українська
locale-traditional-chinese = Традиційна китайська (Тайвань)

# Shared actions and states
action-apply = Застосувати
action-add = Додати
action-back = Назад
action-cancel = Скасувати
action-close = Закрити
action-clear = Очистити
action-confirm = Підтвердити
action-connect = Підключитися
action-continue = Продовжити
action-create = Створити
action-delete = Видалити
action-dismiss = Відхилити
action-done = Готово
action-edit = Редагувати
action-install = Встановити
action-open = Відкрити
action-refresh = Оновити
action-remove = Вилучити
action-reset = Скинути
action-save = Зберегти
action-send = Надіслати
action-sign-in = Увійти
action-sign-out = Вийти
action-update = Оновити версію
state-disabled = Вимкнений
state-enabled = Увімкнений
state-loading = Завантаження…
state-none = Немає

# Foundation and test fixture
language = Мова
language-description = Виберіть мову Smudgy. Текст із сервера не перекладається.
welcome-name = Вітаємо, { $name }.
message-count =
    { $count ->
        [one] { $count } повідомлення
        [few] { $count } повідомлення
        [many] { $count } повідомлень
       *[other] { $count } повідомлення
    }

# Settings window navigation
nav-account = Обліковий запис
nav-preferences = Параметри
nav-audio = Аудіо
nav-security = Безпека
nav-friends = Друзі
nav-clans = Клани
nav-licenses = Ліцензії

# Account and authentication
account-title = Обліковий запис
account-checking = Перевірка статусу облікового запису…
account-verified = підтверджено
account-not-verified = не підтверджено
account-signed-in-as = Ви увійшли як { $handle }
account-signed-in-no-nickname = Ви увійшли (без псевдоніма)
account-signed-in = Ви увійшли.
account-verify-description = Підтвердьте свою електронну адресу, щоб користуватися хмарними функціями (друзі, спільний доступ, синхронізація).
account-code-placeholder = код з електронного листа
account-email-placeholder = електронна пошта
account-verify = Підтвердити
account-email-code = Надіслати мені код на пошту
account-recheck = Перевірити ще раз
account-sign-out-everywhere = вийти скрізь (на всіх пристроях)
account-choose-nickname = Виберіть свій псевдонім
account-nickname-description = Це ваше публічне ім'я. Ваша електронна адреса лишається приватною.
account-nickname-placeholder = псевдонім (3–24 символи: літери, цифри, - або _)
account-claim-handle = Зарезервувати ім'я
account-nickname-change-warning = Зміна псевдоніма змінює те, як вас знаходять інші користувачі.
account-change-nickname = Змінити псевдонім
account-sign-in-description = Ми надішлемо одноразовий код на вашу електронну адресу. Якщо ви тут уперше, обліковий запис буде створено автоматично.
account-have-code = У мене вже є код
account-check-email = Перевірте свою поштову скриньку
account-code-sent = Ми надіслали код на адресу { $email }. Вставте його нижче. Код діє 15 хвилин.
account-resend-code = Надіслати код ще раз
account-error-invalid-email = Вкажіть дійсну електронну адресу.
account-busy-emailing-code = Надсилання коду на пошту…
account-error-paste-code = Вставте код з електронного листа.
account-error-email-for-code = Спочатку вкажіть електронну адресу, щоб ми знали, якого облікового запису стосується код.
account-busy-verifying = Підтвердження…
account-notice-signed-in-needs-nickname = Ви увійшли! Виберіть псевдонім, щоб зарезервувати своє ім'я.
account-notice-signed-in-ready = Ви увійшли.
account-error-invalid-code = Код недійсний або його термін дії минув. Спробуйте запросити новий.
account-error-email-for-resend = Спочатку вкажіть електронну адресу у формі, щоб ми знали, куди його надіслати.
account-busy-sending = Надсилання…
account-notice-code-resent = Якщо до цієї адреси прив'язано обліковий запис, новий код уже в дорозі (він замінить попередній).
account-busy-saving-nickname = Збереження псевдоніма…
account-notice-nickname-changed = Тепер ви { $handle }.
account-notice-signed-out = Ви вийшли.
account-delete = Видалити обліковий запис…
account-delete-title = Видалити ваш обліковий запис?
account-delete-goes = Назавжди буде видалено:
account-delete-goes-maps = • ваші мапи й теки
account-delete-goes-secrets = • секрети, якими ви володієте, і ваші приватні нотатки на будь-якій мапі
account-delete-goes-social = • ваші поширення й дружби
account-delete-goes-clans = • ваше членство в кланах
account-delete-member-secrets = Секрети в клані, останнім власником яких ви є, залишаться — лише для читання тим, хто може їх читати.
account-delete-stays = Пакунки, які ви опублікували, залишаться під своїми назвами.
account-delete-type-nickname = Введіть { $nickname }, щоб підтвердити.
account-delete-confirm = Видалити мій обліковий запис
account-busy-deleting = Видалення облікового запису…
account-delete-done = Ваш обліковий запис видалено, і ви вийшли.
account-delete-signed-out = Ви вийшли. Ваш обліковий запис видаляється; smudgy завершить це сам.
account-delete-last-owner = Ви останній власник: { $clans }. Призначте власником іншого учасника або видаліть клан, а тоді спробуйте ще раз.
account-delete-last-owner-unnamed = Ви останній власник клану. Призначте власником іншого учасника або видаліть клан, а тоді спробуйте ще раз.
account-delete-not-deleted = Обліковий запис не видалено: { $error }
account-delete-unfinished = Видалення облікового запису не завершилося: { $error } Спробуйте ще раз — повторна спроба його завершить.
account-error-nickname-empty = Вкажіть псевдонім.
account-error-nickname-format = Псевдонім має містити 3–24 символи: літери, цифри, «-» або «_».

# Preferences
preferences-title = Параметри
preferences-appearance = Вигляд
preferences-terminal-font = Шрифт терміналу
preferences-font-size = Розмір шрифту
preferences-font-ligatures = Увімкнути лігатури шрифту
preferences-bold-is-bright = Жирний текст терміналу
preferences-bold-is-bright-help = Виберіть, чи жирний SGR-текст використовує більшу насиченість шрифту, яскраву палітру ANSI чи обидва варіанти. Справжні коди яскравих кольорів залишаються яскравими в кожному режимі.
preferences-disable-blink = Вимкнути блимання тексту
preferences-disable-blink-help = Текст, який MUD надсилає як блимкий, не блиматиме.
preferences-bold-mode-bold = Показувати жирний текст жирним
preferences-bold-mode-bright = Показувати жирний текст яскравим
preferences-bold-mode-both = Показувати жирний текст жирним і яскравим
preferences-press-enter = Натисніть Enter, щоб застосувати.
preferences-line-length = Довжина рядка
preferences-wrap-window = переносити за шириною вікна
preferences-line-length-help = Максимальна кількість символів у рядку до перенесення; порожнє поле переносить за шириною вікна. Натисніть Enter, щоб застосувати.
preferences-theme = Тема
preferences-theme-extended-colors = Пристосовувати коди TrueColor і 256-колірні до кольорів теми
preferences-theme-extended-colors-help = Коли цю опцію вимкнено, коди 256-колірної палітри та TrueColor відображаються точно, через що темний текст у темах може бути важко читати, якщо у вас не чорне тло.
preferences-scrollback = Буфер прокручування
preferences-scrollback-help = Кількість рядків, що зберігаються для кожної сесії. Натисніть Enter, щоб застосувати.
preferences-link-tooltip-delay = Затримка підказки посилання (мс)
preferences-link-tooltip-delay-help = Час, протягом якого вказівник має залишатися над посиланням, перш ніж з'явиться підказка. 0 показує її одразу; максимальне значення — 60000. Натисніть Enter, щоб застосувати.
preferences-hide-pane-headers = Приховувати заголовки панелей, коли панель інструментів прихована
preferences-hide-pane-headers-help = Смуги заголовків сесій і панелей видно лише тоді, коли розгорнуто панель інструментів вікна, якщо тільки скрипт не забороняє їх приховувати.
preferences-input = Введення
preferences-command-separator = Роздільник команд
preferences-command-separator-help = Розділяє один рядок введення на кілька команд.
preferences-raw-prefix = Префікс необробленого рядка
preferences-raw-prefix-help = Рядки, що починаються з цього префікса, надсилаються точно так, як їх введено — але без самого префікса. Аліаси та роздільник команд при цьому ігноруються.
preferences-command-input = Поле команди
preferences-command-input-help = Що відбувається з полем введення та його текстом після натискання Enter
preferences-input-select-clear = Виділити все під час надсилання, очистити після втрати фокусу
preferences-input-select = Виділити все під час надсилання
preferences-input-clear = Очистити під час надсилання
preferences-logging = Журналювання
preferences-logging-plain = Зберігати журнали сесій (звичайний текст)
preferences-logging-raw = Зберігати також необроблені журнали (містять коди кольорів ANSI)
preferences-logging-raw-help = Збереження необроблених журналів почнеться з наступного підключення.
preferences-advanced = Додатково
preferences-advanced-scripting = Увімкнути розширені функції скриптів
preferences-advanced-scripting-help = Розблоковує опцію «Вилучити пісочницю» (запуск встановленого пакунка з повним доступом).
preferences-integrations = Інтеграції
preferences-discord-rich-presence = Показувати в Discord гру, у яку ви граєте
preferences-discord-rich-presence-help = Відображає напис «Playing Smudgy» та назву сервера під час підключення через програму Discord на цьому комп'ютері. Налаштування конфіденційності Discord визначають, хто бачитиме ваш статус.
preferences-updates = Оновлення
preferences-auto-updates = Автоматично перевіряти наявність оновлень
preferences-mask-password-input = Приховувати введений текст, коли сервер запитує пароль
preferences-reconnect-on-send-error = Автоматично перепідключатися, коли команду не вдається надіслати
preferences-reconnect-on-send-error-help = Якщо з’єднання обірвалося, а ви надсилаєте команду, Smudgy перепідключається й повертає команду в поле вводу виділеною — натисніть Enter, щоб надіслати її після підключення, або просто друкуйте поверх. Сесії, відкриті офлайн або відключені вами, залишаються офлайн.
preferences-history-case-sensitive-match = Враховувати регістр при пошуку в історії
preferences-history-case-sensitive-match-help = Up/Down перебирає лише записи історії, що починаються з тексту, який залишився невиділеним. Типово вимкнено — пошук без урахування регістру; увімкніть для точного збігу регістру.
preferences-max-history = Розмір історії введення
preferences-max-history-help = Скільки останніх команд запам'ятовує історія Up/Down. 0 — зберігати все.
preferences-invalid-value = некоректне значення

# Theme editor
theme-adjust = Коригування
theme-colors = Кольори
theme-override = Заміна: { $slot }
theme-clear-override = Прибрати заміну
theme-reset-colors = Скинути всі кольори
theme-background = Тло
theme-brightness = Яскравість
theme-contrast = Контраст
theme-saturation = Насиченість
theme-adjust-help = Тло впливає лише на поверхні; Контраст віддаляє текст від тла.
theme-reset-adjustments = Скинути коригування
theme-preview = Чуєш їх, доцю, га? Кумедна ж бо ти, прощайся без ґольфів!

# Security and sessions
security-title = Безпека
security-api-keys = Ключі API
security-new-key = Новий ключ API — скопіюйте його зараз, бо він більше не відображатиметься:
security-key-copied = Готово — скопійовано
security-no-api-keys = Немає ключів API.
security-key-created = створено { $date }
security-last-used = востаннє використано { $date }
security-never-used = ніколи не використовувався
security-revoke = Відкликати
security-create-key = Створити ключ API
security-create-key-help = Для створення ключа потрібна сесія з виконаним входом; ключі показуються лише один раз — під час створення.
security-create-key-login-error = Для створення ключів API потрібно увійти (самого ключа API недостатньо).
security-sessions = Сесії
security-no-sessions = Немає активних сесій.
security-session-created = створено { $date }
security-session-expires = діє до { $date }
security-session-unused = невикористана сесія
security-revoke-current-warning = Відкликання сесії, якою ви зараз користуєтеся, призведе до виходу з облікового запису.

# Friends and licenses
friends-title = Друзі
friends-verify-email = Підтвердьте електронну адресу, щоб знаходити друзів і ділитися з ними мапами
friends-go-account = Перейти до облікового запису

# Client validation labels
validation-invalid-value = некоректне значення

# Server text encoding
encoding-label = Кодування тексту сервера
encoding-utf8 = UTF-8 (рекомендовано)
encoding-big5 = Big5 (старіші сервери з традиційною китайською)
encoding-default = Стандартне (UTF-8)
encoding-help = Вибирайте старіше кодування лише тоді, коли цей MUD не надсилає UTF-8. Узгодження CHARSET має пріоритет над цим параметром.
connection-connecting = Підключення до { $address }…
connection-connected = Підключено.
connection-disconnected = Від'єднано.
connection-lost = З'єднання втрачено
connection-failed = Не вдалося підключитися
connection-decode-warning = Частина тексту сервера не була коректним кодуванням { $encoding } і була замінена.
connection-encode-warning = Команду не надіслано, оскільки деякі символи неможливо подати в кодуванні { $encoding }.
session-action-connect = Підключитися
session-action-reconnect = Підключитися знову
session-action-disconnect = Від'єднатися

# Pane tab strip controls
pane-tab-close = Закрити сесію
pane-tab-hide = Приховати панель
pane-tab-show = Показати панель

# Server and profile connection manager
servers-title = Сервери
servers-loading = Завантаження серверів…
servers-empty = Немає серверів.
servers-new = + Новий сервер
servers-get-started = Додайте сервер, щоб почати
servers-get-started-help = Додайте сервер, а потім профіль, за допомогою якого ви входитимете.
servers-add-first = Додайте свій перший сервер
servers-select = Виберіть сервер зі списку.
server-name = Назва
server-name-placeholder = напр. ArcticMud
server-host = Хост
server-port = Порт
server-port-help = Зазвичай 23 або 4000.
server-wss-url = URL захищеного WebSocket (необов’язково)
server-wss-help = Використовуйте URL WSS замість хоста й порту для Telnet через WebSocket.
server-add = Додати сервер
server-edit = Редагувати сервер
server-delete = Видалити сервер
server-cached-images = Зображення в кеші: { $size }
server-cached-images-pending = Зображення в кеші: …
server-clear-image-cache = Очистити кеш зображень
server-save-add-profile = Зберегти й додати профіль
server-compression = Дозволити стиснення (MCCP2)
server-mccp4-compression = Дозволити стиснення (MCCP4)
server-tls = Безпечне з'єднання (TLS)
server-tls-verify = Перевіряти сертифікат
server-tls-insecure-help = Знімайте позначку лише для серверів із самопідписаними сертифікатами (незахищено).
server-confirm-delete = Справді видалити сервер «{ $name }»? Цю дію не можна скасувати.
server-confirm-delete-action = Так, видалити цей сервер
server-error-port = Некоректний номер порту. Номер порту має бути в діапазоні від 1 до 65535.
server-error-host-empty = Поле «Хост» не може бути порожнім.
server-error-name-empty = Назва сервера не може бути порожньою.
server-error-name-format = Назва сервера може містити лише літери, цифри, підкреслення та дефіси.
server-error-config = Помилка конфігурації: { $error }
server-error-create = Не вдалося створити сервер: { $error }
server-error-update = Не вдалося оновити сервер: { $error }
server-error-delete = Не вдалося видалити сервер: { $error }
server-error-state-changed = Сервер змінився до завершення збереження. Перезавантажте його та повторіть спробу.
server-error-delete-state-changed = Сервер змінився до завершення видалення. Перезавантажте його та повторіть спробу.
server-error-cache-changed = Сервер змінився до очищення кешу зображень. Перевірте його та повторіть спробу.
server-error-cache-clear = Не вдалося очистити кеш зображень: { $error }
server-error-action-delete = Неочікувана помилка: не можна надіслати під час підтвердження видалення.
server-error-action-missing = Неочікувана помилка: немає активної операції.
server-edit-short = Редагувати
server-details-missing = Відомості про сервер не знайдено.

# Observed server metadata (MSSP) on the connect screen
observed-players = { $players ->
    [one] { $players } гравець
    [few] { $players } гравці
    [many] { $players } гравців
   *[other] { $players } гравця
}
observed-last-connected = Останнє з'єднання { $ago }
observed-uptime = { $days ->
    [one] працює { $days } день
    [few] працює { $days } дні
    [many] працює { $days } днів
   *[other] працює { $days } дня
}
observed-tls-available = Доступний TLS
observed-contact = Контакт: { $contact }
observed-link-discord = Discord
observed-link-website = Вебсайт
observed-link-contact = Контакт
ago-just-now = щойно
ago-minutes = { $minutes } хв тому
ago-hours = { $hours } год тому
ago-days = { $days } дн тому

# In-session offer to switch to TLS
tls-offer-body = Цей сервер пропонує зашифроване з'єднання через порт { $port }.
tls-offer-accept = Перемкнутися й підключитися знову
tls-offer-decline = Не для цього сервера

profiles-title = Профілі
profiles-saved-help = Збережені логіни для цього сервера.
profiles-load-error = Не вдалося завантажити профілі для цього сервера.
profiles-error-load = Помилка завантаження профілів для «{ $server }»: { $error }
profiles-new = + Новий профіль
profiles-empty = Збережених профілів поки немає
profiles-restore-last = Відновити останню сесію ({ $profiles })
profile-name = Назва профілю
profile-name-placeholder = Gandalf
profile-description = Опис (необов'язково)
profile-description-placeholder = Чарівник
profile-add = Додати профіль
profile-create = Створити профіль
profile-edit = Редагувати профіль
profile-delete = Видалити профіль
profile-confirm-delete = Справді видалити профіль «{ $name }»?
profile-confirm-delete-action = Так, видалити цей профіль
profile-connect = Підключитися
profile-connect-default = Підключитися з профілем за замовчуванням
profile-offline = Офлайн
profile-on-connect = Після підключення надіслати (необов'язково)
profile-on-connect-placeholder = Gandalf\n$PASSWORD
profile-plaintext-notice = Текст автоматичного входу зберігається на цьому пристрої у відкритому вигляді. Використовуйте $PASSWORD, щоб безпечно підставляти пароль.
profile-keychain-notice = Текст автоматичного входу зберігається у відкритому вигляді; значення $PASSWORD зберігається у зв'язці ключів системи.
profile-password-saved = Пароль збережено
profile-password-change = Змінити
profile-password-clear = Очистити
profile-password-label = Пароль для $PASSWORD
profile-password-placeholder = Пароль
profile-error-no-server = Помилка: сервер не вибрано.
profile-error-password-required = Вкажіть пароль для $PASSWORD або вилучіть $PASSWORD з тексту автоматичного входу.
profile-error-name-empty = Назва профілю не може бути порожньою.
profile-error-name-format = Назва профілю може містити лише літери, цифри, підкреслення та дефіси.
profile-error-config = Помилка конфігурації: { $error }
profile-on-connect-example = Gandalf{ "\u000A" }$PASSWORD
profile-error-action-delete = Неочікувана помилка: не можна надіслати під час підтвердження видалення.
profile-error-action-missing = Неочікувана помилка: немає активної операції.
profile-error-delete-no-server = Помилка: не вибрано сервер для підтвердження видалення.
profile-error-password-clear = Не вдалося очистити збережений пароль: { $error }
profile-error-create = Не вдалося створити профіль: { $error }
profile-error-update = Не вдалося оновити профіль: { $error }
profile-error-delete = Не вдалося видалити профіль: { $error }
profile-error-server-state-changed = Сервер змінився, перш ніж вдалося створити профіль. Перевірте його та повторіть спробу.
profile-error-state-changed = Профіль змінився до завершення збереження. Перевірте його та повторіть спробу.
profile-error-delete-state-changed = Профіль змінився, перш ніж його вдалося видалити. Перевірте його та повторіть спробу.
profile-error-password-state-changed = Профіль змінився, перш ніж вдалося очистити збережений пароль.
profile-warning-password-state-changed = Профіль { $server } / { $profile } збережено, але пароль не змінено, оскільки профіль знову змінився. Перевірте профіль і повторіть спробу.
profile-warning-password-failed = Профіль { $server } / { $profile } збережено, але його пароль не вдалося змінити: { $error }

# Application shell and updates
window-main-development = Smudgy — ВЕРСІЯ ДЛЯ РОЗРОБНИКІВ
window-main-release-candidate = Smudgy — КАНДИДАТ НА ВИПУСК { $version }
window-main-public-test-build = Smudgy — ПУБЛІЧНА ТЕСТОВА ВЕРСІЯ { $version } — зібрана { $build }
window-main-nightly = Smudgy — ЩОДЕННА ВЕРСІЯ { $version } — зібрана { $build }
window-automations = Smudgy — Автоматизації — { $server }
toolbar-connect = Підключитися
toolbar-automations = Автоматизації
toolbar-map-editor = Редактор мап
toolbar-layouts = Розкладки
toolbar-settings = Налаштування

# Named layouts menu
layouts-apply-hint = Натисніть на розкладку, щоб застосувати її.
layouts-empty = Немає збережених розкладок для { $server }.
layouts-overwrite = Перезаписати
layouts-rename = Перейменувати
layouts-save-as = Зберегти поточну як…
layouts-reset = Скинути розкладку панелей
layouts-name-placeholder = Назва розкладки
layouts-confirm-overwrite = Перезаписати розкладку «{ $name }» поточним розташуванням?
layouts-confirm-delete = Видалити розкладку «{ $name }»?
layouts-confirm-reset = Звільнити збережену геометрію панелей цієї сесії та розмістити її панелі заново згідно з поточними визначеннями скриптів?
layouts-saved = Збережено розкладку «{ $name }».
layouts-saved-partial = Збережено розкладку «{ $name }» — { $count ->
        [one] не вдалося додати { $count } збережену картку-заповнювач
        [few] не вдалося додати { $count } збережені картки-заповнювачі
        [many] не вдалося додати { $count } збережених карток-заповнювачів
       *[other] не вдалося додати { $count } збереженої картки-заповнювача
    }.
layouts-save-failed = Не вдалося зберегти розкладку: { $error }
layouts-keep-or-close-intro = Ця розкладка не включає ці сесії. Виберіть, що станеться з кожною:
layouts-keep = Залишити
layouts-close = Закрити
layouts-keep-chosen = ✓ Залишити
layouts-close-chosen = ✓ Закрити
layouts-apply = Застосувати розкладку
shell-no-sessions = Немає активних сесій
shell-connect-help = Виберіть сервер і профіль, щоб підключитися.
audio-output-unavailable = Фізичний аудіовихід недоступний ({ $cause }). Термінал продовжує працювати; типовий вихід Web Audio і sinkId "none" працюють без звуку.
audio-settings-title = Аудіо
audio-panel-keyboard-help = Tab/Shift+Tab переходить між елементами; стрілки змінюють на 5%; Home/End задають межі; Пробіл перемикає вимкнення звуку.
audio-master-label = Загальна гучність
audio-session-label = Сесія {$id} · Сервер {$server} · Профіль {$profile}
audio-ack-preference = налаштування збережено, але не застосовано
audio-ack-failed = відмова виходу; новий стан не застосовано
audio-ack-package-unconfirmed = політику збережено; активну область пакунка не підтверджено
audio-control-row = { $label }: { $volume }%
audio-control-row-muted = { $label }: звук вимкнено
audio-control-row-status = { $row } — { $status }
audio-action-mute = Вимкнути звук
audio-action-unmute = Увімкнути звук
audio-trusted-row = { $label }: керується сесією (довірений Main; без заяви про окреме керування пакунком)
audio-notice-target-closed = Аудіопристрій було закрито; нічого не застосовано й не збережено.
audio-notice-applied-save-failed = Аудіо змінено для цього запуску, але не вдалося зберегти: { $error }
audio-notice-failed = Не вдалося змінити аудіо: { $error }
audio-notice-output-dead = Аудіовихід зупинено. Перезапустіть Smudgy, щоб відновити звук.
audio-notice-preference-saved = Налаштування збережено, але не застосовано цього запуску; перезапустіть.
audio-notice-preference-save-failed = Налаштування аудіо не збережено: { $error }
audio-announcement-applied-save-failed = Аудіо змінено для цього запуску, але не вдалося зберегти.
audio-announcement-failed = Не вдалося змінити аудіо.
audio-announcement-preference-save-failed = Налаштування аудіо не збережено.
shell-connect-action = Підключитися до сервера
shell-verify-email = Підтвердьте свою електронну адресу, щоб користуватися хмарними функціями (друзі, спільний доступ, синхронізація).
shell-open-settings = Відкрити налаштування
shell-client-outdated = Smudgy застарів — деякі функції потребують новішої версії.
shell-download-at = Завантажте його за адресою { $url }
shell-update-available = Доступне оновлення
shell-update-ready = Smudgy { $version } готовий до завантаження.
shell-visit-download = Перейдіть на сторінку завантаження
shell-remind-later = Нагадати пізніше
shell-skip-version = Пропустити цю версію

# Package-update toasts (the main window's bottom pill)
toast-package-updates-ready = Оновлення пакунків готові ({ $count })
toast-package-reload-scripts = Перезавантажити скрипти
toast-package-ignore = Ігнорувати
toast-package-needs-permissions = Оновлення { $name } потребує нових дозволів
toast-package-review = Переглянути…
toast-package-pin-current = Закріпити поточну версію
toast-package-later = Пізніше
toast-package-attention = Оновлень пакунків, що потребують уваги: { $count }
toast-package-open-automations = Переглянути у вікні автоматизацій
toast-package-update-failed = Сталася помилка під час завантаження оновлення для { $name }. Щоб повторити спробу, закрийте та знову відкрийте цю сесію.

# The package-update review modal
package-update-modal-title = Оновлення пакунка
package-update-versions = { $current } → { $latest }
package-update-version-new = Нова версія { $latest }
package-update-asks = Це оновлення потребує додаткових дозволів:
package-update-needs-smudgy = Це оновлення потребує Smudgy { $version } або новішої версії. Спершу оновіть Smudgy.
package-update-grant = Надати дозволи й оновити
package-update-pin = Закріпити поточну версію
package-update-not-now = Не зараз

# Package-update terminal notices (echoed into the owning session)
notice-package-deleted-uninstalled = [package] { $name } було видалено видавцем і деінстальовано
notice-package-update-ready = [package] оновлення { $name } { $from } → { $to } готове
notice-package-staged-ready = [package] { $name } { $to } готово
notice-package-requirements-review = [package] оновлення { $name } треба перевірити, бо змінилися обов'язкові пакунки

# Cloud errors
cloud-error-email-unverified = Підтвердьте свою електронну адресу, щоб користуватися цією функцією.
cloud-error-not-found = Не знайдено.
cloud-error-area-not-found = Мапу не знайдено: { $id }
cloud-error-room-not-found = Кімнату { $room } не знайдено на мапі { $area }.
cloud-error-exit-not-found = Вихід не знайдено: { $id }
cloud-error-label-not-found = Позначку не знайдено: { $id }
cloud-error-shape-not-found = Фігуру не знайдено: { $id }
cloud-error-property-not-found = Властивість «{ $property }» не знайдено в { $entity_type } { $entity_id }.
cloud-error-invalid-input = Некоректні вхідні дані: { $detail }
cloud-error-database = Помилка бази даних: { $detail }
cloud-error-network = Помилка мережі: { $detail }
cloud-error-service-unavailable = Служба мап на мить затримує зміни. Спробуйте ще раз трохи згодом.
cloud-error-serialization = Помилка серіалізації: { $detail }
cloud-error-authentication = Помилка автентифікації: { $detail }
cloud-error-permission = Відмовлено в доступі: { $detail }
cloud-error-internal = Внутрішня помилка: { $detail }
cloud-error-pending = Операції в очікуванні: { $detail }
cloud-error-local-commit-pending = Зміну локальної карти збережено для відновлення, але завершити її не вдалося. Оновіть карти або перезапустіть Smudgy, щоб повторити відновлення.
cloud-error-unauthorized = Ви не увійшли.
cloud-error-unauthorized-detail = Ви не увійшли: { $detail }
cloud-error-name-unavailable = Ім'я недоступне: { $detail }
cloud-error-upgrade-required = Ця версія Smudgy застаріла; оновіть її.
cloud-error-version-unavailable = Цей номер версії вже зайнято; виберіть новий.
cloud-error-version-unavailable-number = Версію { $version } вже зайнято; виберіть нову.
cloud-error-version-not-yanked = Відкличте цю версію, перш ніж видаляти її.
cloud-error-package-name-unavailable = Назва пакунка { $name } уже зайнята. Назви пакунків спільні для всіх і ніколи не використовуються повторно, тож виберіть іншу назву.
cloud-error-body-being-collected = Під час цього вивантаження сервер упорядковував сховище пакунків. Опублікуйте ще раз за хвилину.
cloud-error-too-large = Завеликий обсяг для вивантаження: { $detail }
cloud-error-revision-conflict = Цей елемент змінився на сервері. Оновіть і спробуйте ще раз.
cloud-error-projection-changed = Ваш доступ до цього елемента змінився. Оновіть і спробуйте ще раз.
cloud-error-operation-reused = Цю операцію не вдалося безпечно завершити. Оновіть і спробуйте ще раз.
cloud-error-structural-conflict = Ця зміна суперечить поточній структурі мапи: { $detail }
cloud-error-merge-areas-no-sources = Виберіть принаймні одну вихідну область для об’єднання.
cloud-error-merge-areas-same-area = Кожна вихідна область має бути вказана лише один раз і відрізнятися від цільової.
cloud-error-merge-areas-no-rooms = Виберіть принаймні одну кімнату з кожної частково об’єднуваної області або не вказуйте вибір кімнат, щоб об’єднати всю область.
cloud-error-merge-areas-room-not-found = Вибраної кімнати немає у вихідній області. Перевірте номери кімнат перед об’єднанням.
cloud-error-merge-areas-mixed-tiers = Усі задіяні області, зокрема області з посиланнями на об’єднувані, мають використовувати однакове сховище: локальне або сеансу.
cloud-error-merge-areas-unsupported-storage = Це сховище не підтримує об’єднання областей. Використовуйте підтримуване локальне сховище або сховище сеансу.
cloud-error-merge-areas-busy = В одній із задіяних областей є незавершені зміни або триває інша операція. Завершіть або врегулюйте ці операції перед об’єднанням.
cloud-error-merge-areas-source-changed = Одна із задіяних областей змінилася під час підготовки об’єднання. Перегляньте поточну мапу й спробуйте ще раз.
cloud-error-merge-areas-room-numbers-exhausted = У цільовій області недостатньо доступних номерів кімнат для цього об’єднання. Виберіть іншу цільову область.
cloud-error-merge-areas-invalid-translation = Зсув або кінцева позиція виходить за підтримуваний діапазон. Використовуйте скінченні координати й рівні в межах 32-бітного цілого числа.
cloud-error-merge-secret-room-data = Секрет зберігає дані для кімнати { $room }. Спершу перемістіть або видаліть їх.
cloud-error-secret-area-level = Секретами керують у редакторі мапи.
cloud-error-secret-view-only = Цей секрет доступний лише для перегляду.
cloud-error-secret-unavailable = Цей секрет вам більше не доступний.
cloud-error-secret-cannot-add = Ви не можете додавати до цього секрету.
cloud-error-secret-cannot-edit = Ви не можете змінювати вміст цього секрету.
cloud-error-secret-cannot-remove = Ви не можете нічого видаляти з цього секрету.
cloud-error-secret-linked-map-rooms = Секрет клану на мапі, підключеній до клану посиланням, зберігає лише власні кімнати. Він не може тримати дані в кімнатах мапи, вести до них чи обмінюватися кімнатами з мапою.
cloud-error-secret-link-between-secrets = Зв’язок не може з’єднувати два секрети.
cloud-error-secret-link-into-other-map = Зв’язок із Секретом іншої мапи веде до однієї з його кімнат, і жоден зв’язок не веде до приватних доповнень іншої мапи.
cloud-error-secret-map-exit-retarget = Вихід мапи не може вести до кімнати секрету. Створіть зв’язок у секреті.
cloud-error-move-busy = Мапа ще зберігається. Спробуйте ще раз за мить.
cloud-error-move-drops-places = Мапа сесії не вмістить Секретів чи приватних доповнень цієї мапи, тож перемістити її туди без їх утрати не можна. Натомість скопіюйте її; оригінал їх збереже.
cloud-error-move-splits-links = З'єднання поєднує ці кімнати з кімнатами, що лишаються. Перемістіть їх разом або спершу видаліть з'єднання.
cloud-error-room-number-exists = Номер кімнати там уже зайнятий. Нічого не переміщено.
cloud-error-invalid-connection = Це з'єднання некоректне: { $detail }

# Friends, blocks, and ownership transfers
social-title = Друзі
social-add-friend = Додати друга
social-nickname-placeholder = псевдонім
social-send-request = Надіслати запит
social-enter-nickname = Вкажіть псевдонім.
social-no-nickname-match = Немає користувача з таким псевдонімом.
social-sending-request = Надсилання запиту…
social-request-sent = Запит надіслано.
social-incoming-requests = Отримані запити
social-no-incoming-requests = Немає отриманих запитів.
social-outgoing-requests = Надіслані запити
social-no-outgoing-requests = Немає надісланих запитів.
social-pending = (очікує)
social-accept = Прийняти
social-decline = Відхилити
social-ownership-transfers = Передача власності
social-incoming-transfer = { $from } хоче передати вам «{ $subject }»
social-outgoing-transfer = «{ $subject }» запропоновано користувачу { $to }
social-outgoing-transfer-clan = «{ $subject }» запропоновано клану { $clan }
social-no-friends = Немає друзів.
social-blocks = Блокування
social-block = Заблокувати
social-blocking = Блокування…
social-confirm-block = Підтвердити блокування
social-block-warning = Блокування не сповіщає іншу особу. Воно скасовує будь-яку дружбу та припиняє весь спільний доступ між вами, в обидва боки.
social-no-blocked-users = Немає заблокованих користувачів.
social-unblock = Розблокувати
social-block-note = Заблоковані користувачі не бачать цього запису; розблокування ніколи не відновлює наданий доступ.
social-friend-since = у друзях з { $date }
social-unfriend-warning = Точно видалити з друзів? Це припинить увесь спільний доступ до мап між вами — в обидва боки.
social-keep-friend = Залишити
social-unfriend = Видалити з друзів
social-no-nickname = (без псевдоніма)

# Clans
cloud-error-last-owner = Спершу призначте власником когось іншого.
cloud-error-clan-not-empty = У теках цього клану ще є мапи. Перенесіть або видаліть їх, а потім видаліть клан.
cloud-error-clan-dissolving = Цей клан розпускається: жодна мапа не може до нього потрапити, а його учасники, власники та мапи учасників лишаються незмінними, доки розпуск не завершиться.
cloud-error-atlas-not-empty = Спершу перенесіть або видаліть мапи з цієї теки.
cloud-error-already-member = Ця особа вже є учасником.
cloud-error-name-in-use = Ця назва вже використовується.
cloud-error-transfer-already-pending = На це вже чекає пропозиція; водночас може бути лише одна.
clans-title = Клани
clans-verify-email = Підтвердьте електронну адресу, щоб приєднуватися до кланів.
clans-name-placeholder = назва клану
clans-enter-name = Введіть назву.
clans-invitations = Запрошення
clans-invited-you = { $inviter } запрошує вас до клану «{ $clan }»
clans-invited-you-by-a-member = Один з учасників запрошує вас до клану «{ $clan }»
clans-secret-offers = Пропозиції власності секретів
clans-secret-offer = { $initiator } пропонує вам власність секрету «{ $secret }» у клані «{ $clan }»
clans-secret-offer-from-a-member = Один з учасників пропонує вам власність секрету «{ $secret }» у клані «{ $clan }»
clans-secret-offer-joint = Разом із: { $others }
clans-secret-offer-accepted = Ви прийняли; очікуємо на: { $others }.
clans-empty = Кланів ще немає.
clans-owner-badge = власник
clans-member-count = { $count ->
    [one] { $count } учасник
    [few] { $count } учасники
    [many] { $count } учасників
   *[other] { $count } учасника
}
clans-members = Учасники
clans-invite = Запросити
clans-invitation-sent = Запрошення надіслано.
clans-invite-failed = Не вдалося запросити { $name }.
clans-remove-confirm = Вилучити { $name } з клану «{ $clan }»?
clans-groups = Групи
clans-group-name-placeholder = назва групи
clans-no-groups = Груп ще немає.
clans-filter-placeholder = фільтр
clans-delete-group = Видалити групу…
clans-delete-group-confirm = Видалити «{ $name }»? Її учасники втратять те, чим із нею поділилися.
clans-delete-clan = Видалити клан…
clans-delete-clan-confirm = Видалити «{ $name }»? Усі вийдуть із клану, і це не можна скасувати. Спершу його теки мають бути порожніми.
clans-leave-clan = Покинути клан…
clans-leave-confirm = Покинути «{ $name }»? Ви втратите доступ до його мап.
clans-leave = Покинути

# Набори дозволів
presets-kind-map = Мапи
presets-kind-folder-curation = Порядок у теках
presets-kind-clan-secrets = Секрети власності клану
presets-kind-secret = Секрет
presets-kind-package = Пакунки
presets-kind-clan-administration = Адміністрування клану
presets-reader = Читач
presets-contributor = Дописувач
presets-editor = Редактор
presets-curator = Куратор
presets-access-manager = Керівник доступу
presets-drafter = Автор чернеток
presets-maintainer = Супровідник
presets-recruiter = Вербувальник
presets-officer = Офіцер
presets-group-lead = Лідер групи
presets-folder-manager = Керівник теки
presets-custom = Власний набір
presets-scope-clan = Увесь клан
presets-scope-groups = Групи
presets-scope-folders = Теки
presets-scope-maps = Мапи
presets-scope-packages = Пакунки
presets-action-clan-edit-profile = Зміна даних клану
presets-action-clan-read-members = Перегляд учасників
presets-action-clan-invite = Запрошення
presets-action-clan-revoke-invitation = Відкликання запрошень
presets-action-clan-remove-member = Вилучення учасників
presets-action-group-create = Створення груп
presets-action-group-rename = Перейменування груп
presets-action-group-delete = Видалення груп
presets-action-group-assign = Вибір учасників груп
presets-action-group-inspect = Перегляд учасників груп
presets-action-access-inspect = Перегляд того, хто має доступ
presets-action-access-manage = Надання доступу
presets-action-folder-create = Створення тек
presets-action-folder-read = Перегляд теки
presets-action-folder-rename = Перейменування тек
presets-action-folder-accept-filing = Переміщення мап у теку
presets-action-folder-accept-transfer = Переносити мапи до цієї папки
presets-action-map-create = Створення мап
presets-action-map-create-member-owned = Створення мап власності учасників
presets-action-map-read = Читання
presets-action-map-add = Додавання вмісту
presets-action-map-edit = Зміна вмісту
presets-action-map-remove = Вилучення вмісту
presets-action-map-rename = Перейменування мап
presets-action-map-refile = Переміщення мап між теками
presets-action-map-delete = Видалення мап
presets-action-map-copy = Копіювання мап
presets-action-map-share-external = Поширення поза кланом
presets-action-secrets-create-member-owned = Створення секретів власності учасників
presets-action-secrets-create-clan-owned = Створення секретів власності клану
presets-action-secrets-read = Читання секретів
presets-action-secrets-add = Додавання до секретів
presets-action-secrets-edit = Зміна секретів
presets-action-secrets-remove = Вилучення із секретів
presets-action-secrets-manage-access = Керування тим, хто читає секрети
presets-action-secrets-copy = Копіювання секретів разом із мапою
presets-action-package-create = Створення пакунків
presets-action-package-read = Перегляд пакунка
presets-action-package-edit-draft = Зміна чернетки
presets-action-package-edit-metadata = Зміна опису
presets-action-package-publish = Публікація версій
presets-action-package-retire = Вилучення версій з обігу
presets-action-package-availability = Зробити публічним чи приватним
presets-action-package-delete = Видалення пакунка
presets-action-secret-read = Читання
presets-action-secret-add = Додавання
presets-action-secret-edit = Зміна
presets-action-secret-remove = Вилучення
presets-action-secret-manage-access = Керування доступом
presets-action-secret-copy = Копіювання разом із мапою

# Settings › Clans
clans-all-clans = Усі клани
clans-tab-members = Учасники
clans-tab-groups = Групи
clans-tab-access = Доступ
clans-tab-settings = Налаштування
clans-you = Ви
clans-column-member = Учасник
clans-column-groups = Групи
clans-owner-chip = Власник клану
clans-edit-groups = Змінити групи
clans-page-range = { $first }–{ $last } з { $total }
clans-page-previous = Попередня
clans-page-next = Наступна
clans-members-hidden = Цей клан показує список учасників лише тим, кого обере.
clans-invited = Запрошені
clans-pending-badge = Очікує
clans-revoke = Відкликати
clans-invite-title = Запросити до клану { $clan }
clans-invite-groups = Запропоновані групи
clans-member-groups-title = Групи · { $name }
clans-remove-member = Вилучити учасника
clans-self-assign-reason = Лише власник клану може додати вас до групи, яку створив хтось інший.
clans-new-group = Нова група
clans-new-group-note = Ви приєднуєтеся до груп, які створюєте, і вибираєте, хто ще в них буде.
clans-edit-group = Змінити групу
clans-group-name-label = Назва
clans-group-color-label = Колір
clans-group-permissions = Дозволи
clans-group-members = Учасники
clans-group-owners-note = Власники клану.
clans-group-no-permissions = Ця група не має додаткових дозволів.
clans-group-permissions-view-only = Доступ групам надають власники клану та ті, кому вони це дозволять.
clans-group-roster-hidden = Ви не бачите, хто в цій групі.
clans-shown-count = Показано: { $count }
clans-add-member = Додати учасника
clans-add-member-title = Додати учасника · { $group }
clans-add-member-none = Нікого додати.
clans-add-permission = Додати дозвіл
clans-administration-button = Адміністрування клану…
clans-remove-permission-confirm = Вилучити цей дозвіл? Група одразу втратить те, що він дає.
clans-note-clan = Лише керування кланом і групами. Доступ до мап, тек, Секретів і пакунків налаштовується на цих ресурсах.
clans-note-folders = Охоплює мапи, додані туди пізніше.
clans-note-may-grant = Може надавати: { $preset }
clans-note-delegated = Надано тим, кому власники клану дозволили надавати доступ.
clans-scope-more = { $count ->
    [one] ще { $count }
    [few] ще { $count }
    [many] ще { $count }
   *[other] ще { $count }
}
clans-scope-names-and-more = { $names } і ще { $count }
clans-unknown-group = Певна група
clans-access-maps = Мапи
clans-access-packages = Пакунки
clans-access-secrets = Секрети
clans-access-no-maps = Немає мап і тек, до яких ви тут маєте доступ.
clans-access-no-packages = Немає пакунків, до яких ви тут маєте доступ.
clans-access-no-secrets = Немає секретів, які ви читаєте в цьому клані.
clans-access-pick = Виберіть щось ліворуч, щоб побачити, хто має до цього доступ.
clans-kind-folder = Тека
clans-kind-map = Мапа
clans-kind-package = Пакунок
clans-open-map = Відкрити мапу
clans-open-package = Відкрити пакунок
clans-open-share = Вікно поширення…
clans-outside-shares = Поза кланом { $clan }
clans-outside-shares-note = Друзі, які читають цю мапу, лише для перегляду. Поширення закінчується, коли той, хто поширив, покидає клан або втрачає право поширювати.
clans-outside-shares-none = Не поширено поза кланом.
clans-outside-shared-by = Поширив(ла): { $name }
clans-in-folder = У теці { $folder }
clans-group-assignments = Призначені групи
clans-no-assignments = Жодна група ще не має до цього доступу.
clans-your-access = Що ви тут можете:
clans-assign-group = Призначити групу
clans-through-clan = Від усього клану
clans-through-folder = Від теки { $folder }
clans-through-a-folder = Від однієї з тек
clans-member-owned = Власність учасників
clans-clan-owned = Власність клану
clans-secret-clan-owned-note = Групи, яким надано секрети власності клану на його мапі чи в теці, теж його читають; див. Мапи.
clans-details = Дані клану
clans-name-label = Назва
clans-description-label = Опис
clans-description-placeholder = Для чого цей клан
clans-permission-none = Немає
clans-editor-title-add = Додати дозвіл · { $group }
clans-editor-title-assign = Призначити групу · { $resource }
clans-editor-title-admin = Призначити групу · Адміністрування клану
clans-editor-title-edit = Змінити дозвіл
clans-editor-group = Група
clans-editor-member = Учасник
clans-editor-member-note = Окремому учаснику можна надати дозволи адміністрування клану й пакунків; мапи й секрети надають групам.
clans-editor-scope = Стосується
clans-editor-targets-empty = Тут поки нічого вибрати.
clans-editor-permissions = Дозволи
clans-editor-separate = Ніколи не в наборі
clans-editor-may-grant = Може надавати
clans-editor-may-grant-help = Може надавати не більше такого доступу до мап, у тих самих місцях.
clans-editor-all-groups = Усі групи
clans-editor-group-scope = Межі керування групами
clans-editor-group-scope-help = Зміна групи й вибір її учасників — окремі дозволи.
clans-editor-choose-group = Виберіть групу.
clans-editor-choose-target = Виберіть хоча б одне.
clans-editor-choose-action = Виберіть хоча б один дозвіл.

# Clans in the map editor
clan-maps-a-clan = клан
clan-maps-a-group = група
clan-maps-incoming-badge = вхідні: { $count }
clan-maps-new-folder = Нова тека
clan-maps-incoming = Вхідні мапи…
clan-maps-new-folder-title = Нова тека в клані { $clan }
clan-maps-create = Створити
clan-maps-delete-folder-question = Видалити «{ $name }»?
clan-maps-delete-folder-detail = Спершу перенесіть її мапи до іншої теки.
clan-maps-share-clan-title = Поділитися всім кланом { $clan }
clan-maps-share-clan-help = Кожен, кого ви оберете, отримає доступ до всіх мап у ньому, зокрема нових.
clan-maps-share-folder-help = Кожен, кого ви оберете, отримає доступ до всіх мап у цій теці, зокрема нових.
clan-maps-everyone = Усі
clan-maps-filter-groups-placeholder = фільтр груп
clan-maps-no-groups = Жодна група не підходить.
clan-maps-incoming-title = Вхідні мапи · { $clan }
clan-maps-incoming-empty = Жодна мапа не чекає.
clan-maps-given-by = передає { $owner }
clan-maps-given-folder = Тека «{ $name }»
clan-maps-given-help = Мапа, передана клану, стає власністю клану; мапа, що лишається за тим, хто її передав, стає власністю учасників, і лише він вирішує, хто її бачить. Її Секрети лишаються за учасником, який її передав.
clan-maps-could-not-accept = Не вдалося прийняти. Можливо, пропозицію відкликано.
clan-maps-folder-placeholder = Тека
clan-maps-accept = Прийняти
clan-maps-decline = Відхилити
clan-maps-gone = Мапи «{ $name }» більше немає.
clan-maps-given-stays = від { $owner } · лишається за ним
clan-maps-offered-help = Учасники пропонують клану мапи, які їм належать. Після прийняття мапа належить клану, і на неї поширюється доступ теки.
clan-maps-offered-by = пропонує { $owner }
clan-maps-a-member = учасник
clan-share-put-in-clan = Помістити в клан…
clan-share-give-to-clan = Передати клану
clan-share-give-to-clan-help = Власники клану спільно володіють цією мапою
clan-share-stays-mine = Лишається моєю
clan-share-stays-mine-help = Перемістіть цю мапу до клану, зберігши право власності на неї.
clan-share-give-folder-to-clan-help = Власники клану спільно володіють цими мапами
clan-share-folder-stays-mine-help = Перемістіть ці мапи до клану, зберігши право власності на них.
clan-share-title = Поділитися «{ $name }»
clan-share-owned-by-clan-in = Власність { $clan } · у теці { $folder }
clan-share-owned-by-clan = Власність { $clan }
clan-share-owned-by = Власники: { $owners }
clan-share-member-owned = Власність учасників
clan-share-member-owned-reach = Доступ теки та власники клану не поширюються на цю мапу. Діє лише те, чим її власники діляться тут із групами та учасниками.
clan-share-frozen = Ніхто не може змінити, хто бачить цю мапу: обліковий запис її останнього власника видалено. Наявний доступ лишається.
clan-share-you-can = Ви можете: { $actions }
clan-share-you-can-read = Ви можете переглядати цю мапу.
clan-share-you = ви
clan-share-clan-owners = Власники клану
clan-share-nobody-yet = Поки що ніхто не має доступу через групу чи учасника.
clan-share-from-several-maps = разом з іншими мапами
clan-share-from-folder = з теки { $folder }
clan-share-from-clan = з усього клану { $clan }
clan-share-change-for-folder = Змінити для теки…
clan-share-remove = Вилучити
clan-share-remove-question = Вилучити цей доступ до мапи?
clan-share-includes-future-members = Охоплює майбутніх учасників
clan-share-chosen-automatically = Усі, кого вона охоплює, автоматично
clan-share-chosen-by-owners = Її учасників обирають лише власники клану
clan-share-chosen-by-owners-and-leads = Її учасників обирають власники клану та лідери групи
clan-share-add-recipient = + Група / учасник
clan-share-pick-recipient = Група чи учасник
clan-share-groups-only = Показано лише групи: ви не можете бачити список учасників.
clan-share-outside = Поза кланом { $clan }
clan-share-outside-help = Друзі поза кланом бачать лише цю мапу: тільки перегляд, ніколи її Секрети.
clan-share-shared-by = поділився { $name }
clan-share-shared-by-you = поділилися ви
clan-share-pick-friend = Друг
clan-share-share-view-only = Поділитися для перегляду
clan-share-outside-refused = Не вдалося поділитися. Можливо, ця людина в клані або вже не ваш друг.
clan-share-secrets = Секрети на цій мапі
clan-share-share-secret = Поділитися «{ $name }»…
clan-share-give-up = Відмовитися від власності…
clan-share-give-up-confirm = Відмовитися від власності на «{ $name }»? Ви збережете лише те, чим поділилися з вами чи вашими групами; без цього ви більше її не бачитимете.
clan-share-only-owner = Ви її єдиний власник. Спершу запропонуйте власність комусь іншому.
clan-share-needs-another-owner = Спершу потрібен інший власник.
clan-share-offer-help = Кожен приймає окремо; власниками вони стають, коли прийме останній. Прийняти можуть лише учасники, які бачать мапу.
clan-share-offer-replace = Зробити їх єдиними власниками
clan-share-send-offer = Надіслати пропозицію
clan-share-offer-sent = Пропозицію надіслано.
clan-share-no-candidates = Немає кому це запропонувати.
clan-share-accepted = { $name } (прийнято)
clan-share-waiting-for = { $name } (очікує)
clan-share-offer-to-clan = Запропоновано клану { $clan }
clan-share-cancel-offer = Скасувати пропозицію
clan-share-make-clan-owned = Передати клану…
clan-share-make-clan-owned-confirm = Запропонувати «{ $name }» клану { $clan }? Після прийняття мапа належить клану: її власники перестають нею володіти, і на неї поширюється доступ теки. Те, чим ви тут поділилися, лишається.
clan-share-make-clan-owned-action = Передати клану
clan-share-someone-accepts = Хтось, хто опрацьовує вхідні мапи клану, прийме її до теки.
clan-share-into-folder = До теки
clan-share-offered-to-clan = Запропоновано клану. Вона стане власністю клану, коли її прийме той, хто опрацьовує вхідні мапи клану.
clan-share-ownership-refused = Не вдалося змінити власника. Можливо, його тим часом змінили; спробуйте ще раз.
clan-share-make-member-owned = Передати учасникам…
clan-share-make-member-owned-help = Запропонуйте її учасникам: коли всі приймуть, вона належатиме їм, і на неї поширюватиметься лише те, чим вони поділяться.
clan-share-lose-folder-access = Ці втратять доступ, який давали їм тека та клан: { $names }.
clan-share-put-title = Помістити «{ $name }» у клан
clan-share-friend-shares-end = Її спільний доступ для друзів припиняється: { $names }.
clan-share-put-secrets = Її Секрети лишаються вашими, як ваші Секрети в клані.
clan-share-put-action = Помістити в клан
clan-share-put-done = Тепер вона в клані { $clan }.
clan-share-new-map-clan-owned = Власність клану
clan-share-new-map-member-owned-choice = Власність учасників
clan-share-new-map-member-owned-help = Вона ваша, і ви обираєте, хто її бачить. Доступ теки та власники клану на неї не поширюються.
clan-share-new-map-member-owned = Вона буде власністю учасників: вона ваша, і ви обираєте, хто її бачить.
clan-map-offers = Пропозиції власності мап
clan-map-offer = { $initiator } пропонує вам власність мапи «{ $map }» у клані { $clan }.
clan-map-offer-replace = { $initiator } пропонує вам власність мапи «{ $map }» у клані { $clan } замість її нинішніх власників.
clan-map-offer-from-a-member = Один з учасників пропонує вам власність мапи «{ $map }» у клані { $clan }.
clan-map-offer-replace-from-a-member = Один з учасників пропонує вам власність мапи «{ $map }» у клані { $clan } замість її нинішніх власників.
clans-leave-copy-maps = Спершу скопіювати мапи, якими я володію в цьому клані, до моїх мап

# Map editor dialogs and sharing
mapper-transfer-owner-friend-only = Передати можна лише те, що належить вам, і лише наявному другові.
mapper-transfer-offer-sent = Пропозицію надіслано. «{ $subject }» буде передано, коли отримувач прийме її на своїй панелі «Друзі».
mapper-transfer-give = Передайте «{ $subject }» другові.
mapper-transfer-warning = Після прийняття ви станете адміністратором, а не власником.
mapper-route-preview-accept = Прийняти маршрут
mapper-copy-boundary-one = { $count } граничне з'єднання залишає виділені кімнати.
mapper-copy-boundary-many = { $count ->
        [one] { $count } граничне з'єднання залишає виділені кімнати.
        [few] { $count } граничні з'єднання залишають виділені кімнати.
       *[other] { $count } граничних з'єднань залишає виділені кімнати.
    }
mapper-copy-boundary-help = Типово вони пропускаються. Якщо їх додати, кожне стає висячим одностороннім з'єднанням без збережених точок маршруту.
mapper-copy-include-boundary = Додати з'єднання, що виходять за межі виділення
mapper-copy-selection-title = { $action } виділення
action-cut = Вирізати
action-copy = Копіювати
mapper-transfer-leaves-folder = Якщо те, що ви хочете надіслати, міститься в теці, воно буде надіслане з теки після прийняття пропозиції.
mapper-transfer-to = Кому передати
mapper-transfer-give-or-clan = Передайте «{ $subject }» другові або одному зі своїх кланів.
mapper-transfer-warning-clan = Усі наявні спільні доступи до цієї мапи буде видалено після завершення перенесення до клану.
mapper-transfer-clans = Ваші клани
mapper-transfer-offer-sent-clan = «{ $subject }» переміщено до { $clan }.
mapper-transfer-owner-clan-only = Ви маєте володіти цією мапою або папкою й мати дозвіл на перенесення до вибраного місця в клані.
mapper-loading-friends = Завантаження друзів…
mapper-no-friends-transfer = Поки що немає друзів.
mapper-filter-placeholder = фільтрувати…
mapper-sending = Надсилання…
mapper-send-offer = Надіслати пропозицію
mapper-new-area = Нова мапа
mapper-name-new-area = Назвіть нову мапу
mapper-area-name-placeholder = назва мапи
mapper-delete-area = Видалити мапу
mapper-delete-area-question = Видалити «{ $name }» та { $rooms ->
        [one] { $rooms } кімнату в ній
        [few] { $rooms } кімнати в ній
        [many] { $rooms } кімнат у ній
       *[other] { $rooms } кімнати в ній
    }?
mapper-cannot-undo = Цю дію не можна скасувати.
mapper-new-folder = Нова тека
mapper-name-new-folder = Назвіть нову теку
mapper-folder-name-placeholder = назва теки
mapper-save-in = Зберегти в
mapper-save-cloud = Хмара — можна ділитися
mapper-save-local = На цьому пристрої
mapper-save-local-signed-out = Збережено на цьому пристрої. Увійдіть, щоб створювати теки в хмарі, які синхронізуються між пристроями та дають змогу ділитися вмістом.
mapper-delete-folder = Видалити теку
mapper-delete-folder-question = Видалити теку «{ $name }»?
mapper-folder-empty = Ця тека порожня.
mapper-folder-maps-go =
    { $count ->
        [one] Спершу її { $count } мапу буде переміщено до іншої теки.
        [few] Спершу її { $count } мапи буде переміщено до іншої теки.
       *[many] Спершу її { $count } мап буде переміщено до іншої теки.
    }
mapper-folder-maps-move-along =
    { $count ->
        [one] Її { $count } мапу буде переміщено разом із нею.
        [few] Її { $count } мапи буде переміщено разом із нею.
       *[many] Її { $count } мап буде переміщено разом із нею.
    }
mapper-move-area-to = Перемістити «{ $name }» до:
mapper-move-to-folder = Перемістити до теки
mapper-save-to-folder = Зберегти до теки
mapper-save-area-in = Зберегти «{ $name }» у:
mapper-folder-section = Тека
mapper-folder-new-option = Нова тека…
mapper-folder-none-yet = Тут ще немає теки. Назвіть теку для цієї мапи.
mapper-folder-none = У вас ще немає тек. Назвіть теку для цієї мапи.
mapper-folder-create-and-move = Створити й перемістити
mapper-folder-create-and-save = Створити й зберегти
mapper-loose-maps-failed = Деякі мапи поза теками не вдалося покласти в теку. Smudgy спробує ще раз наступного разу.
mapper-relocation-duplicate-notice = { $error } — Створено копію «{ $name }» у місці призначення, але оригінал не вдалося видалити, і він може містити новіші зміни. Збережіть оригінал і узгодьте вміст обох копій перед повторним переміщенням.
mapper-share-folder-title = Поділитися текою «{ $name }»
mapper-loading = Завантаження…
mapper-local-move-title = Перемістити спільні мапи з хмари?
mapper-local-move-shared-warning = Переміщення цих мап до локального сховища припинить спільний доступ до них у хмарі. Інші користувачі втратять доступ, а їхні Секрети та Приватні доповнення, збережені в хмарних оригіналах, буде видалено. Ваші мапи, Секрети та Приватні доповнення буде збережено локально. Цю дію не можна скасувати.
mapper-copy-duplicate-intro = Створює другу копію «{ $name }».
mapper-copy-shared-intro = Створює вашу власну редаговану копію «{ $name }»
mapper-copy-name-placeholder = назва вашої копії
mapper-copy-duplicate-inactive = Дублікат початково неактивний. Вам потрібно активувати його, щоб він став видимим для скриптів.
mapper-copy-atlas-offer = Бажаєте скопіювати всю теку?
mapper-copy-whole-atlas = Скопіювати всю теку…
mapper-copying = Копіювання…
mapper-copy = Копіювати
mapper-duplicate-map = Дублювати мапу
mapper-copy-to-my-maps = Копіювати до моїх мап
mapper-transfer-title = Передати «{ $name }»
mapper-show-on-servers = Показувати на серверах
mapper-show-name-on = Показувати «{ $name }» на:
mapper-no-server-entries = Немає записів серверів.
mapper-unchecked-all-servers = Якщо не позначено жодного сервера, мапа видима на всіх серверах.
mapper-recipients = Отримувачі
mapper-filter-handle-placeholder = фільтрувати за іменем
mapper-no-friends-share = Поки що немає друзів.
mapper-no-friends-filter = Жоден друг не відповідає цьому імені.
mapper-they-can = Отримувачі можуть
mapper-can-edit-area = Може редагувати
mapper-can-edit-folder = Може редагувати
mapper-can-reshare = Може повторно надавати доступ (може передати доступ на читання на один рівень углиб)
mapper-can-copy-area = Може копіювати (копії стають їхніми)
mapper-can-copy-folder = Може копіювати (копії стають їхніми)
mapper-make-admin-area = Надати права адміністратора
mapper-disclose-servers = Розкрити сервери
mapper-disclose-servers-help = Отримувачі бачать ці назви серверів, завдяки чому їхній клієнт може автоматично розмістити мапи у відповідній грі.
mapper-shared-with = Доступ надано користувачу { $recipient }.
mapper-share-error = Не вдалося надати доступ користувачу { $recipient } — { $error }
mapper-share-failed = Не вдалося надати доступ користувачу { $recipient }.
mapper-now-sees-map = { $recipient } тепер бачить мапу «{ $map }».
mapper-secret-map-hint = Його бачать лише ті, хто бачить мапу «{ $map }».
mapper-has-access = має доступ
mapper-clan-members-hidden = Показано лише групи: ви не можете бачити список учасників.
mapper-secret-can-add = Може додавати
mapper-secret-can-edit = Може редагувати
mapper-secret-can-share = Може надавати доступ
mapper-secret-can-copy = Може копіювати
mapper-secret-level-copies = { $level }, може копіювати
mapper-copy-secrets-along = { $count ->
    [one] До копії потрапляють лише секрети, які ви можете скопіювати: { $count } секрет.
    [few] До копії потрапляють лише секрети, які ви можете скопіювати: { $count } секрети.
    [many] До копії потрапляють лише секрети, які ви можете скопіювати: { $count } секретів.
   *[other] До копії потрапляють лише секрети, які ви можете скопіювати: { $count } секрету.
}
mapper-copy-secrets-none = До копії потрапляють лише секрети, які ви можете скопіювати: жоден із тих, що ви тут бачите.
mapper-sharing = Надання доступу…
mapper-share = Поділитися
mapper-who-has-access = Хто має доступ
mapper-not-shared = Ще нікому не надано доступ.
mapper-badge-edit = редагування
mapper-badge-reshare = повторне надання
mapper-badge-copy = копіювання
mapper-badge-view = перегляд
mapper-badge-add = додавання
mapper-badge-share = надання доступу
mapper-badge-atlas = тека
mapper-shared-by-you = надано вами
mapper-shared-via = через { $handle }
mapper-shared-by = надано користувачем { $handle }
mapper-shared-by-owner = надано власником
mapper-edit-flags = Редагувати прапорці
mapper-revoke = Відкликати
mapper-flag-admin = адмін
mapper-remove-reshare-warning = Скасування повторного надання доступу також відкликає все, чим ця особа повторно поділилася.
mapper-saving = Збереження…
mapper-revoke-warning = Відкликає доступ і все, чим ця особа повторно поділилася.
mapper-revoke-secret-warning = Відкликає доступ до «{ $name }».
mapper-revoke-atlas-warning = Цей дозвіл охоплює всю теку.
mapper-revoking = Відкликання…
mapper-folder-share-help = Кожен, кого ви виберете, отримає кожну мапу в цій теці, зокрема мапи, які ви додасте до неї пізніше.
mapper-make-admin-folder = Надати права адміністратора
mapper-revoke-folder-warning = Відкликає доступ до кожної мапи в цій теці та все, чим ця особа повторно поділилася. Копії, які вона вже зробила, залишаються в її розпорядженні.
mapper-could-not-revoke = Не вдалося відкликати доступ — можливо, дозвіл уже видалено.
mapper-could-not-update-grant = Не вдалося оновити — можливо, дозвіл видалено або його зміна не дозволена.
mapper-this-folder = ця тека

# Mapper enum choices
direction-north = Північ
direction-east = Схід
direction-south = Південь
direction-west = Захід
direction-up = Вгору
direction-down = Вниз
direction-northeast = Північний схід
direction-northwest = Північний захід
direction-southeast = Південний схід
direction-southwest = Південний захід
direction-in = Всередину
direction-out = Назовні
direction-special = Спеціальний
direction-other = Інший
exit-style-normal = Звичайний
exit-style-dashed = Штриховий
exit-style-dotted = Крапковий
exit-style-meandering = Звивистий
exit-style-stub = Обрубок
shape-type-rectangle = Прямокутник
shape-type-rounded-rectangle = Прямокутник із заокругленими кутами
alignment-left = Ліворуч
alignment-center = По центру
alignment-right = Праворуч
alignment-top = Вгорі
alignment-bottom = Внизу

# Map inspector
inspector-invalid-value = некоректне значення
inspector-properties = Властивості
inspector-tags = Теги
inspector-value-placeholder = значення
inspector-name-placeholder = назва
inspector-add-tag-placeholder = додати тег
inspector-tag-to = до
inspector-tag-on-rooms = { $count ->
    [one] у { $count } кімнаті
    [few] у { $count } кімнатах
    [many] у { $count } кімнатах
   *[other] у { $count } кімнатах
}
inspector-tag-too-long = Тег може мати щонайбільше { $limit } символів.
inspector-tag-hint-map = { $tag } — тег місця { $place }. Якщо додати його до мапи, його побачить кожен, хто читає мапу.
inspector-tag-hint-place = { $tag } — тег місця { $place }. Якщо додати його до місця { $destination }, його побачить кожен, хто читає «{ $destination }».
inspector-tag-adds-to = Додасть до { $count } з { $total } кімнат.
inspector-tag-cant-carry = { $count ->
    [one] { $count } кімната в { $place } не може мати тегів місця { $destination }.
    [few] { $count } кімнати в { $place } не можуть мати тегів місця { $destination }.
    [many] { $count } кімнат у { $place } не можуть мати тегів місця { $destination }.
   *[other] { $count } кімнати в { $place } не можуть мати тегів місця { $destination }.
}
inspector-tag-used-on-map = Використовується на мапі
inspector-tag-used-in = Використовується в
inspector-tag-added = { $count ->
    [one] Додано { $tag } до { $count } кімнати.
   *[other] Додано { $tag } до { $count } кімнат.
}
inspector-tag-added-some-had = { $count ->
    [one] Додано { $tag } до { $count } кімнати (вже мали: { $already }).
   *[other] Додано { $tag } до { $count } кімнат (вже мали: { $already }).
}
inspector-room-heading = Кімната #{ $number }
inspector-place-room-heading = { $place } #{ $number }
inspector-title = Заголовок
inspector-room-title-placeholder = заголовок кімнати
inspector-description = Опис
inspector-room-description-placeholder = опис кімнати
inspector-level = Рівень
inspector-color = Колір
inspector-default-placeholder = (стандартний)
inspector-none-placeholder = (немає)
inspector-exits = Виходи
inspector-unknown-map = Невідома мапа
inspector-connection-missing = З'єднання більше не існує
inspector-endpoint-from = Від
inspector-endpoint-to = До
inspector-connection-level-anchored = Трикутник у напрямку виходу (вгору/вниз)
inspector-connection-port-invalid = зсув порту має бути в межах від 0 до 1
inspector-appearance = Вигляд
side-north = Північ
side-east = Схід
side-south = Південь
side-west = Захід
inspector-connection-port-placeholder = порт 0–1
inspector-connection-auto = Авто
inspector-connection-redistribute = Перерозподілити
inspector-connection-route = Маршрут
inspector-connection-orthogonal = Ортогональний
inspector-connection-reroute = Прокласти маршрут заново…
inspector-connection-route-stale = Маршрут може бути застарілим після змін мапи; скористайтеся опцією «Прокласти маршрут заново».
inspector-connection-route-collision = Маршрут перетинає кімнату; скористайтеся опцією «Прокласти маршрут заново» або відредагуйте його вручну.
inspector-connection-route-invalid = Збережений автоматичний маршрут некоректний; скористайтеся опцією «Прокласти маршрут заново».
inspector-connection-route-inactive = У цьому режимі збережений маршрут неактивний.
inspector-connection-clear-route = Очистити збережений маршрут
inspector-css-color-placeholder = колір CSS
inspector-connection-reset = Скинути маршрут і вигляд
inspector-width = Ширина
inspector-height = Висота
inspector-label = Позначка
inspector-label-text = Текст
inspector-label-text-placeholder = текст позначки
inspector-background = Тло
inspector-font-size = Розмір шрифту
inspector-font-weight = Товщина шрифту
inspector-alignment = Вирівнювання
inspector-shape = Фігура
inspector-fill = Заповнення
inspector-stroke = Обведення
inspector-stroke-width = Товщина обведення
inspector-corner-radius = Радіус заокруглення
inspector-selected = виділено: { $count }
inspector-selection-counts = з'єднання: { $links }, кімнати: { $rooms }, позначки: { $labels }, фігури: { $shapes }
inspector-mixed-placeholder = (змішано)
inspector-set-color = Встановити колір (Enter, щоб застосувати)
inspector-set-level = Встановити рівень (Enter, щоб застосувати)
inspector-active = Активна
inspector-inactive = Неактивна
inspector-active-help = Активні мапи використовуються для визначення вашого розташування під час гри.
inspector-copies = Копії цієї мапи
inspector-this-map-suffix = { $name } (ця мапа)
inspector-use-only-copy = Використовувати лише цю копію
inspector-multiple-copies-warning = Одночасно активними можуть бути кілька копій. Коли ви відвідуєте кімнати, копії яких є в кількох активних мапах, мапер може розмістити вас у непередбачуваному місці.
inspector-no-area-selected = Мапу не вибрано
inspector-shared-map = спільна мапа
inspector-copied-from = Скопійовано з { $source }
inspector-copy-revision = у ревізії { $revision }
inspector-copy-date = від { $date }
inspector-source-changed = (джерело відтоді змінилося)
inspector-view-only = { $attribution } — лише для читання.
inspector-room-position = Рівень { $level } · ({ $x }, { $y })
inspector-area-properties = Властивості мапи
inspector-shape-summary = { $width }×{ $height } у ({ $x }, { $y })
inspector-entities-selected = виділено елементів: { $count }

# Map list: several maps and folders chosen at once
mapper-multi-maps =
    { $count ->
        [one] { $count } мапа
        [few] { $count } мапи
        [many] { $count } мап
       *[other] { $count } мапи
    }
mapper-multi-folders =
    { $count ->
        [one] { $count } тека
        [few] { $count } теки
        [many] { $count } тек
       *[other] { $count } теки
    }
mapper-multi-maps-object =
    { $count ->
        [one] { $count } мапу
        [few] { $count } мапи
        [many] { $count } мап
       *[other] { $count } мапи
    }
mapper-multi-folders-object =
    { $count ->
        [one] { $count } теку
        [few] { $count } теки
        [many] { $count } тек
       *[other] { $count } теки
    }
mapper-multi-and = { $first } і { $second }
mapper-multi-quoted = «{ $name }»
mapper-multi-list-separator = {", "}
mapper-multi-skipped = Пропущено: { $names }
mapper-multi-clear = Зняти виділення
mapper-multi-move = Перемістити { $items } до теки…
mapper-multi-share = Надати { $items } у спільний доступ…
mapper-multi-transfer = Передати { $items }…
mapper-multi-servers = Показувати { $items } на серверах…
mapper-multi-delete = Видалити { $items }…
mapper-multi-delete-none = Видалити…
mapper-multi-delete-question = Видалити { $items }? Цього не можна скасувати.
mapper-multi-delete-title = Видалити { $items }
mapper-multi-delete-holding =
    { $count ->
        [one] У «{ $folder }» є ще { $count } мапа, яку ви не вибрали: { $names }.
        [few] У «{ $folder }» є ще { $count } мапи, які ви не вибрали: { $names }.
       *[many] У «{ $folder }» є ще { $count } мап, які ви не вибрали: { $names }.
    }
mapper-multi-leftovers-move = Перемістити їх до теки
mapper-multi-leftovers-delete = Видалити їх теж
mapper-multi-delete-error = Не вдалося видалити { $names } — { $error }
mapper-multi-kind-map = мапа
mapper-multi-kind-local-map = локальна мапа
mapper-multi-kind-session-map = мапа сесії
mapper-multi-kind-shared-map = спільна мапа
mapper-multi-kind-folder = тека
mapper-multi-kind-local-folder = локальна тека
mapper-multi-kind-shared-folder = спільна тека
mapper-multi-move-to = Перемістити { $items } до:
mapper-multi-share-title = Надати { $items } у спільний доступ
mapper-multi-shared = Доступ до «{ $name }» надано.
mapper-multi-share-failed = Не вдалося надати доступ до «{ $name }» користувачу { $recipient }.
mapper-multi-share-error = Не вдалося надати доступ до «{ $name }» користувачу { $recipient } — { $error }
mapper-multi-transfer-title = Передати { $items }
mapper-multi-transfer-leaves-folder = Мапа з теки вийде з неї після прийняття пропозиції.
mapper-multi-send-offers = Надіслати пропозиції
mapper-multi-offer-sent = Пропозицію щодо «{ $name }» надіслано.
mapper-multi-offer-error = Не вдалося надіслати пропозицію щодо «{ $name }» — { $error }
mapper-multi-show-on = Показувати { $items } на:
mapper-multi-some = частина

# Mapper toolbar, area list, and window
mapper-tool-select = Виділити
mapper-tool-add-room = Додати кімнату
mapper-tool-add-label = Додати позначку
mapper-tool-add-shape = Додати фігуру
mapper-level-down = Рівень нижче
mapper-level = Рівень { $level }
mapper-level-up = Рівень вище
mapper-undo = Скасувати
mapper-redo = Повторити
mapper-duplicate = Дублювати
mapper-transfer-action = Передати…
mapper-syncing = синхронізація { $count }
mapper-sync-failed = невдалих: { $count }
mapper-sync = Синхронізувати
mapper-sync-tip = Синхронізувати з хмарою
mapper-status-saved = Збережено
mapper-status-saving = Збереження змін: { $count }
mapper-status-offline = Офлайн, змін в очікуванні: { $count }
mapper-status-held = { $count ->
    [one] Очікування служби мап (1 зміна в черзі)
    [few] Очікування служби мап ({ $count } зміни в черзі)
    [many] Очікування служби мап ({ $count } змін у черзі)
   *[other] Очікування служби мап ({ $count } зміни в черзі)
}
mapper-status-conflict = Конфлікт потребує перегляду
mapper-status-could-not-save = Не вдалося зберегти
mapper-pending-tip = Зміни чекають на цьому пристрої, доки не збережуться.
mapper-tool-link = З'єднати кімнати (Ctrl: в один бік)
mapper-menu-rename = Перейменувати
mapper-menu-save = Зберегти…
mapper-menu-move-to-folder = Перемістити до теки…
mapper-menu-delete = Видалити мапу…
mapper-add-to = Додати до
mapper-place-map = Мапа
mapper-place-private = Приватне
mapper-secrets = Секрети
mapper-rooms = Кімнати
mapper-tags = Теги
mapper-tags-none = Тегів поки немає.
mapper-rooms-filter = Фільтр за назвою, місцем, номером або тегом
mapper-rooms-none = Жодна кімната не підходить.
mapper-rooms-more = { $count ->
    [one] Ще { $count } кімната. Звузьте список фільтром.
    [few] Ще { $count } кімнати. Звузьте список фільтром.
   *[other] Ще { $count } кімнат. Звузьте список фільтром.
}
mapper-room-untitled = (без назви)
mapper-new-secret = Новий секрет
mapper-secret-name-placeholder = Назва секрету
mapper-secret-color = Колір
mapper-secret-color-automatic = Автоматичний колір
mapper-room-count = { $count ->
    [one] { $count } кімната
    [few] { $count } кімнати
    [many] { $count } кімнат
   *[other] { $count } кімнати
}
mapper-view-only = лише перегляд
mapper-private-help = Їх бачите лише ви.
mapper-place-gone = «{ $name }» більше немає.
mapper-now-editing = Зараз редагуєте
mapper-panel-kind-map = МАПА
mapper-panel-kind-atlas = АТЛАС
mapper-panel-data-fields = Поля даних
mapper-panel-maps = Мапи
mapper-panel-shares = Спільний доступ
mapper-panel-servers = Сервери
mapper-panel-no-maps = У ньому ще немає мап.
mapper-panel-storage-cloud = У хмарі
mapper-panel-storage-local = На цьому пристрої
mapper-panel-storage-session = Лише в цій сесії
mapper-badge-secret = Секрет
mapper-badge-private = Приватне
mapper-delete-secret = Видалити секрет…
mapper-delete-secret-question = Видалити «{ $name }» разом з усім вмістом?
mapper-now-viewing = Перегляд
mapper-secret-owner = Власник
mapper-secret-owner-me = Я
mapper-secret-owner-members = Учасники
mapper-secret-owner-clan = Клан
mapper-secret-owner-members-of = Учасники клану { $clan }
mapper-secret-owner-clan-named = Клан { $clan }
mapper-secret-owner-members-help = Він належить вам; згодом ви можете запропонувати власність іншим учасникам.
mapper-secret-owner-clan-help = Він належить власникам клану. Ви починаєте як дописувач; хто ще його читає, визначає доступ у клані.
mapper-secret-owner-none = Ви не можете створити секрет на цій мапі.
mapper-secret-member-owned = Власність учасників · { $clan }
mapper-secret-clan-owned = Власність клану · { $clan }
mapper-secret-read-only = Ви можете читати цей секрет, але не змінювати його.
mapper-secret-level-reader = Читач
mapper-secret-level-contributor = Дописувач
mapper-secret-level-editor = Редактор
mapper-secret-level-access-manager = Керівник доступу
mapper-secret-level-manages = { $level }, керує доступом
mapper-access = Доступ
mapper-access-owner = Власник
mapper-access-clan-owner = Власник клану
mapper-access-grant = { $level }, надано напряму
mapper-access-group = { $level } через групу «{ $group }»
mapper-access-group-unnamed = { $level } через групу
mapper-access-clan-grants = { $level } через доступ клану до мапи
mapper-access-you = { $name } (ви)
mapper-access-only-yours = Усіх читачів бачать лише ті, хто керує доступом.
mapper-ownership = Власність
mapper-offer-ownership = Запропонувати власність…
mapper-offer-pick = Запропонувати власність:
mapper-offer-nobody = Цей секрет поки не читає ніхто інший.
mapper-offer-replace-toggle = Вони замінять поточних власників
mapper-offer-add-help = Вони стануть власниками поряд із поточними.
mapper-offer-replace-help = Вони стануть єдиними власниками.
mapper-offer-from-clan-help = Вони стануть власниками, а власники клану перестануть ними бути.
mapper-offer-joint-help = Кожен приймає окремо; власність зміниться, коли прийме останній.
mapper-offer-recipient-limit = Пропозиція може назвати щонайбільше { $count } осіб.
mapper-offer-send = Надіслати пропозицію
mapper-offer-cancel = Відкликати пропозицію
mapper-offer-accepted = { $name } (прийнято)
mapper-offer-waiting = { $name } (очікує)
mapper-offer-add = Пропозиція спільної власності
mapper-offer-replace = Пропозиція передати власність
mapper-offer-to-clan = Пропозиція передати клану
mapper-owners = Власники
mapper-owner-inactive = { $name } (уже не в клані)
mapper-owner-remove = Вилучити…
mapper-owner-remove-confirm = Вилучити { $name } з власників «{ $secret }»? Ця людина збереже лише те, чим із нею поділилися.
mapper-owner-remove-action = Вилучити
mapper-owner-give-up = Відмовитися від власності…
mapper-owner-give-up-confirm = Відмовитися від власності на «{ $secret }»? Ви збережете лише те, чим поділилися з вами чи вашими групами; без цього ви втратите до нього доступ.
mapper-owner-give-up-action = Відмовитися від власності
mapper-owner-only-you = Ви його єдиний власник. Спершу запропонуйте власність комусь іншому.
mapper-owner-last = Спершу потрібен інший власник.
mapper-make-clan-owned = Передати клану…
mapper-make-clan-owned-pick = Хто прийме його від імені клану:
mapper-make-clan-owned-confirm = Запропонувати «{ $secret }» клану? Коли { $name } прийме пропозицію, він стане власністю клану: власники клану зможуть робити з ним усе, а його теперішні власники, зокрема ви, перестануть ними бути.
mapper-make-clan-owned-confirm-self = Передати «{ $secret }» у власність клану? Власники клану, зокрема ви, зможуть робити з ним усе, а інші його теперішні власники перестануть ними бути. Те, чим поділилися з учасниками й групами, залишається.
mapper-make-clan-owned-action = Передати клану
mapper-ownership-refused = Не вдалося змінити власника «{ $secret }». Можливо, його тим часом змінили; спробуйте ще раз.
mapper-map-rooms = Кімнати мапи
mapper-menu-paste-here = Вставити тут
mapper-menu-add-point-here = Додати точку тут
mapper-menu-remove-point = Видалити точку
mapper-menu-move-to = Перемістити до
mapper-place-several = Кілька
mapper-move-title = Перемістити до «{ $place }»
mapper-move-action = Перемістити
mapper-moving = Переміщення…
mapper-move-loses-data = «{ $place }» втрачає свої дані про ці кімнати.
mapper-move-strands-exits = Виходи з інших мап до цих кімнат вестимуть у нікуди.
mapper-move-reveals = Їх побачить кожен, хто бачить цю мапу.
mapper-move-splits = #{ $room } з'єднана з #{ $other }, яка лишається в «{ $place }».
mapper-move-include-linked = Додати пов'язані кімнати
mapper-move-conflict = Нічого не переміщено: «{ $from }» або «{ $to }» щойно змінили деінде. Мапа вже показує цю зміну; перевірте її та перемістіть ще раз.
mapper-no-room-numbers = На цій мапі закінчилися номери кімнат.
mapper-route-finding = Пошук маршруту…
mapper-route-ready = Маршрут готовий.
mapper-route-none = Вільного маршруту не знайдено.
mapper-route-limit = Не вдалося знайти маршрут вчасно.
mapper-route-invalid = Маршрут перетнув кімнату. Спробуйте ще раз.
mapper-route-link-changed = З'єднання змінилося. Спробуйте ще раз.
mapper-route-map-changed = Мапа змінилася. Спробуйте ще раз.
mapper-route-access-changed = Ви більше не можете редагувати цю мапу.
mapper-route-same-level = Автоматичні маршрути з'єднують дві кімнати на одній мапі й рівні.
mapper-link-area-gone = Цієї мапи вже немає, тож з'єднання не створено.
mapper-link-room-taken = Кімнату #{ $old } щойно зайняли. З'єднання тепер використовує #{ $new }.
mapper-link-not-queued = Не вдалося зберегти з'єднання. Спробуйте ще раз.
mapper-link-two-secrets = З'єднання не може поєднувати два секрети.
mapper-map-view-only = Ця мапа доступна лише для перегляду.
mapper-map-cannot-add = Ви не можете додавати до цієї мапи.
mapper-map-cannot-edit = Ви не можете змінювати вміст цієї мапи.
mapper-map-cannot-remove = Ви не можете нічого видаляти з цієї мапи.
mapper-history-cleared-elsewhere = Історію скасування очищено: кімнати на цій мапі переміщено в іншому вікні.
mapper-paste-links-skipped = Не вдалося приєднати тут скопійовані з'єднання: { $count }.
mapper-paste-too-large = Це забагато, щоб вставити за раз.
mapper-edits-not-recovered = Деякі незбережені зміни не вдалося відновити.
mapper-selection-removed = Хтось інший видалив виділений елемент.
inspector-route-too-many-points = Цей маршрут має забагато точок, щоб зробити його прямокутним.
legend-move-freely = рухати вільно
legend-cancel = скасувати
legend-snap-port = прив'язати до середини або кутів
legend-read-only = Лише перегляд
legend-move-point = перемістити точку
legend-remove-point = видалити точку
legend-stop-editing = завершити редагування
legend-move-port = перемістити порт
legend-slide-port = зсунути вздовж стіни
legend-add-point = додати точку
legend-key-drag = Перетягнути
legend-key-delete = Delete
routing-stub = Коротка
routing-simple = Пряма
routing-manual = Ручна
routing-automatic = Автоматична
segments-direct = Прямий
corners-sharp = Гострі кути
corners-rounded = Заокруглені кути
dash-solid = Суцільна
dash-dashed = Штрихова
dash-dotted = Пунктирна
inspector-in = У
mapper-shared-by-friend = Надано другом
mapper-a-friend = друг
mapper-shared-by-owner-pair = Надано користувачем { $sharer } · власник: { $owner }
mapper-shared-by-person = Надано користувачем { $person }
mapper-window-title = Smudgy — редактор мап
mapper-window-area-title = Smudgy — редактор мап — { $area }
mapper-copy-report = Скопійовано мапи: { $copied }; пропущено: { $skipped } (не можна скопіювати).
mapper-copy-rooms-denied = Власник цієї мапи не дозволив копіювати кімнати.
mapper-create-first-room = Створити першу кімнату
mapper-create-first-room-help = Потім виберіть її розташування на сітці.
mapper-save-keep-mine = Зберегти мою
mapper-save-keep-theirs = Зберегти їхню
action-retry = Повторити спробу
mapper-local-maps-signin = Локальні мапи зберігаються на цьому пристрої. Увійдіть, щоб користуватися мапами в хмарі, які синхронізуються між пристроями та якими можна ділитися.
mapper-sign-in-create = Увійдіть або створіть обліковий запис
mapper-copy-unavailable = Копіювання цієї мапи недоступне.
area-list-title = Мапи
area-list-new-map = Нова мапа
area-list-new-folder = Нова тека
area-list-session-maps = Мапи сесії
area-list-my-local-maps = Мої локальні мапи
area-list-my-shared-maps = Мої спільні мапи
area-list-not-in-folder = Поза текою
area-list-on-server = На { $server }
area-list-unassigned = Непризначені
area-list-other-servers = Інші сервери
area-list-this-server = Цей сервер ({ $server })
area-list-all-atlases = Усі теки
area-list-empty = порожньо
area-list-new-map-folder = Нова мапа в теці
area-list-rename-folder = Перейменувати теку
area-list-delete-folder = Видалити теку
area-list-share-action = Поділитися…
area-list-servers-action = Сервери…
area-list-inactive-tip = Не використовується для визначення вашого розташування під час гри
area-list-copy-badge = копія
area-list-copy-family-tip = Одна з кількох копій тієї самої мапи — відкрийте її, щоб вибрати активну копію
area-list-owned-by = власник: { $owner }
area-list-reshared = Повторно надано: { $sharer } поділився мапою, власником якої є { $owner }
area-list-admin-badge = адмін
area-list-edit-badge = редагування
area-list-move-action = Перемістити…
area-list-shared-folder = Спільна тека
area-list-shared-by = Надано користувачем { $person }
area-list-default-badge = Типова
area-list-default-tip = Сюди потрапляють нові мапи зі скриптів на { $server }, якщо вони не вказують теку
area-list-use-for-new-maps = Використовувати для нових мап на { $server }
area-list-use-for-new-maps-failed = Не вдалося змінити, куди потрапляють нові мапи: { $error }

# Automations shell, dashboard, and command palette
automations-title = Автоматизації
automations-connected = Підключено
automations-profile = Профіль · { $server } ({ $host })
automations-active = Активні
automations-errors = Помилки
automations-disabled = Вимкнені
automations-packages = Пакунки
automations-create = Створити
automation-alias = Аліас
automation-trigger = Тригер
automation-hotkey = Гаряча клавіша
automation-folder = Тека
automation-module = Модуль
automation-package = Пакунок
automations-discover = Огляд
automations-discover-help = Перегляньте хмару Smudgy в пошуках пакунків для встановлення.
automations-see-more = Показати більше →
automations-private-shared = Приватні та спільні
automations-store = Крамниця
automations-reload = Перезавантажити
automations-state-unavailable = Smudgy не вдалося прочитати всі дані автоматизації. Виправте файли, а потім перезавантажте їх.
automations-inspect = Інспектувати
automations-inspect-help = Відкрити інспектор скриптів для активної сесії цього сервера (потрібна підключена сесія з увімкненим зневадженням)
automations-modules = Модулі
automations-session-store = Сховище сесії
palette-group-create = Створити
palette-group-go = Перейти
palette-group-move = Перемістити
palette-group-jump = Перейти до
palette-create-alias = Створити аліас
palette-create-trigger = Створити тригер
palette-create-hotkey = Створити гарячу клавішу
palette-create-folder = Створити теку
palette-create-module = Створити модуль
palette-create-package = Створити пакунок
palette-discover-packages = Огляд пакунків
palette-private-shared = Приватні та спільні пакунки
palette-session-store = Сховище сесії та події
palette-reload-scripts = Перезавантажити всі скрипти
palette-move-top = Перемістити { $subject } на головний рівень
palette-move-folder = Перемістити { $subject } до { $folder }
palette-kind-folder = тека
palette-kind-package = пакунок
palette-input-placeholder = Введіть команду або виконайте пошук…
palette-escape = Esc

# Session store inspector
store-title = Сховище сесії
store-description = Перегляд у реальному часі оновлень стану, подій і повідомлень, опублікованих у цій сесії
store-waiting = Очікування на перший знімок сесії…
store-published-state = Опублікований стан
store-empty = Ще нічого не опубліковано. Стан з'явиться тут тієї миті, коли скрипт або пакунок його встановить.
store-usage = записи: { $entries } · { $bytes }
store-more-hidden = … ще { $count } не показано
store-catalogue = Стан, події та повідомлення
store-catalogue-empty = Ще не виявлено жодного дескриптора interop. Тут з'являться оголошені дескриптори, згенеровані події та надіслані повідомлення.
store-provenance-declared = оголошений
store-provenance-declared-unseen = оголошений · не виявлений у цій сесії
store-provenance-runtime = створений під час виконання
store-provenance-undeclared = спостережений · неоголошений
store-shape = форма: { $shape }
store-declared-shape = оголошена форма: { $shape }
store-truncated = обрізано
time-just-now = щойно
time-seconds-ago = { $count } с тому
time-minutes-ago = { $count } хв тому
time-hours-ago = { $count } год тому

# Automations sidebar
automations-new = Новий
automations-search-placeholder = Пошук автоматизацій…
terminal-search-placeholder = Пошук у терміналі…
automations-all = Усі
automations-aliases = Аліаси
automations-triggers = Тригери
automations-hotkeys = Гарячі клавіші
automations-folders = Теки
automations-modules-plural = Модулі
automations-packages-plural = Пакунки
automations-create-new = Створити { $kind }
automations-scripts = Скрипти
automations-local = Локальні
automations-show-more = Показати ще { $count }…

# Automation editors
editor-top-level = (верхній рівень)
editor-failed-read = Не вдалося прочитати «{ $path }»: { $error }
editor-failed-save-folders = Не вдалося зберегти теки: { $error }
editor-name-empty = Назва не може бути порожньою
editor-name-in-use = Назва вже використовується
editor-failed-save = Не вдалося зберегти: { $error }
editor-script-missing = Скрипт «{ $name }» більше недоступний.
editor-saved = Збережено { $name }.
editor-failed-save-delete = Не вдалося зберегти після видалення: { $error }
editor-deleted = Видалено { $name }.
editor-folder-created = Теку створено.
editor-failed-save-scripts = Не вдалося зберегти скрипти: { $error }
editor-failed-save-module = Не вдалося зберегти модуль: { $error }
editor-file-changed-outside = Цей файл змінено поза Smudgy. Відкрийте його знову та перевірте зміни перед збереженням.
editor-module-saved = Модуль збережено.
editor-failed-modules-dir = Не вдалося знайти каталог модулів: { $error }
editor-failed-create-module = Не вдалося створити модуль: { $error }
editor-module-created = Створено модуль { $name }.
editor-create-module = Створити модуль
automation-inspector-unavailable = Інспектор ще недоступний — запускається, коли сесія підключиться. Підключіть цю сесію ще раз, а потім знову натисніть «Інспектувати».
editor-delete = Видалити
editor-unsaved = Незбережені зміни
editor-discard = Відхилити
editor-send-text = Надіслати як текст
editor-text = Текст
editor-script = Скрипт
editor-folder = Тека
editor-new-alias = Новий аліас
editor-new-hotkey = Нова гаряча клавіша
editor-new-trigger = Новий тригер
editor-new-folder = Нова тека
editor-new-module = Новий модуль
editor-kind-in-folder = { $kind } · у теці { $folder }
editor-kind-top-level = { $kind } · верхній рівень
editor-name = Назва
editor-pattern = Шаблон
editor-behavior = Поведінка
editor-shortcut = Комбінація
editor-patterns-invalid = Щонайменше один шаблон не компілюється. Перевірте підсвічені рядки.
editor-patterns = Зіставляти рядки як
editor-add-pattern = Додати ще один шаблон
editor-create-alias = Створити аліас
editor-create-hotkey = Створити гарячу клавішу
editor-create-trigger = Створити тригер
editor-no-match = ✗ немає збігу
editor-enter-line = · введіть рядок, який може надіслати гра
editor-would-fire = ✓ спрацьовує
editor-enter-command = · введіть команду для перевірки
editor-folder-summary = Тека · кількість елементів: { $count }
editor-path = Шлях
editor-folder-disabled-help = Вимкнено — скрипти в цій теці не запускатимуться. Використовуйте «/», щоб вкладати теки.
editor-folder-help = Використовуйте «/», щоб вкладати теки. Перемістіть скрипт до теки за допомогою поля Тека в редакторі (або палітри команд).
editor-folder-path-conflict = Тека вже використовує цей шлях або шлях міститься всередині теки, яку ви переміщуєте: { $path }
editor-folder-missing = Цієї теки більше немає: { $path }
editor-contents = Вміст
editor-delete-folder-question = Видалити цю теку?
editor-move-scripts-parent = Перемістити скрипти до батьківської теки
editor-delete-scripts-too = Видалити також скрипти
editor-create-folder = Створити теку
editor-module-help = Локальні модулі завантажуються подібно до пакунків, але працюють з повними правами й без пісочниці
editor-source = Джерело
activation-title = Увімкнені профілі
activation-enable-everywhere = Увімкнути всюди
activation-disable-everywhere = Вимкнути всюди
activation-current-profile = Поточний
activation-every-profile = Кожен профіль
activation-no-profile = Немає профілів
activation-profile-count = { $total ->
        [one] { $enabled } з { $total } профілю
       *[other] { $enabled } з { $total } профілів
    }
activation-profile-list-unavailable = Список профілів недоступний
activation-profile-inventory-error = Smudgy не вдалося прочитати всі профілі. Виправте файли профілів і перезавантажте їх, перш ніж змінювати окремі профілі.
activation-folder-state-error = Smudgy не вдалося прочитати активацію тек. Виправте файл packages.json і перезавантажте його, перш ніж змінювати активацію.
activation-folder-error-blocked = Виправте помилку теки, показану вище, перш ніж змінювати активацію.
activation-module-state-error = Smudgy не вдалося прочитати активацію модулів. Виправте файл налаштувань модулів і перезавантажте його, перш ніж змінювати активацію.
automation-folder-case-ambiguous = Цій назві відповідає кілька збережених тек. Перейменуйте один із варіантів, що відрізняються лише регістром, перш ніж змінювати активацію.
automation-folder-state-unavailable = Smudgy не може зберегти автоматизації, оскільки не вдалося прочитати packages.json. Виправте файл і перезавантажте його.
activation-create-profile = Створіть профіль, щоб вибрати його тут.
activation-folder-masked = Цю теку вимкнено, тому що теку { $ancestor } вимкнено для цього профілю.
module-tab-settings = Налаштування
module-tab-source = Джерело
module-nested-load-help = Вкладені модулі типово не запускаються. Інші модулі можуть їх імпортувати.
package-tab-about = Про пакунок
package-tab-settings = Налаштування
package-tab-permissions = Дозволи
package-tab-manifest = Маніфест
package-tab-sharing = Спільний доступ
package-required-in-profile = Потрібен пакунку { $package }.
package-parameter-values = Налаштування пакунка
package-parameter-global = Однакові налаштування для всіх профілів
package-parameter-profile = Окремі налаштування для кожного профілю
package-parameter-profile-label = Профіль
package-parameter-profile-status = Проблеми
package-parameter-missing = Відсутні: { $params }
package-parameter-scope-updated = Область налаштувань оновлено.
package-copy-settings = Копіювати налаштування до…
package-copy-settings-title = Копіювати налаштування до іншого профілю
package-copy-settings-help = Копіює налаштування профілю { $profile } до профілю, який ви виберете, замінюючи його поточні значення.
package-copy-settings-destination = Профіль призначення
package-copy-settings-choose = Виберіть профіль
package-settings-copied = Налаштування скопійовано до профілю { $profile }.
package-settings-copy-failed = Smudgy не може скопіювати налаштування: { $error }
automation-discard-and-switch = Відкинути зміни й перейти
automation-discard-and-close = Відкинути зміни й закрити
automations-created-count =
    { $count ->
        [one] { $count } автоматизація
        [few] { $count } автоматизації
        [many] { $count } автоматизацій
       *[other] { $count } автоматизації
    }
automations-dependency-count =
    { $count ->
        [one] { $count } залежність
        [few] { $count } залежності
        [many] { $count } залежностей
       *[other] { $count } залежності
    }
module-script-extension-required = Файл модуля має закінчуватися на .js, .ts, .jsx або .tsx.
module-import-only-help = Smudgy не запускає цей файл безпосередньо. Модуль сценарію може його імпортувати.
module-save-before-activation = Збережіть зміни перед зміною профілів, у яких цей модуль активний.
package-save-before-activation = Збережіть зміни пакунка, перш ніж увімкнути його в інших профілях.
module-created-automations = Створені автоматизації
package-name-locked-published = Після публікації назву цього пакунка змінити не можна. Створіть копію з новою назвою.
package-publication-status-checking = Перевірка історії публікацій…
package-not-published = Не опубліковано
package-local-not-found = Локальний пакунок «{ $name }» не знайдено.
package-publication-status-unknown = Увійдіть, щоб перед перейменуванням підтвердити, що цей пакунок не публікували.
package-publication-record-invalid = Smudgy не може перевірити запис про публікацію цього пакунка. Перейменування недоступне: { $error }
package-publication-record-conflict = Локальний запис про публікацію ({ $local }) не відповідає пакунку в хмарі ({ $cloud }). Перейменування та спільний доступ недоступні.
package-publication-record-save-failed = Smudgy знайшов опублікований пакунок, але не зміг зберегти локальний запис про публікацію. Перейменування залишається недоступним: { $error }
package-published-state-unavailable = Smudgy не може завантажити цей опублікований пакунок із поточного облікового запису. Його назва залишається заблокованою.
package-rename-publication-not-confirmed = Перед перейменуванням Smudgy має підтвердити, що пакунок не опубліковано.
package-global-source-needed = Значення профілів відрізняються. Виберіть профіль, значення якого Smudgy має використовувати глобально.
package-global-source-selected = Вибране джерело: { $profile }
package-use-profile-globally = Використовувати цей профіль глобально
package-remote-leaf-conflict = Smudgy виявив суперечливих віддалених власників пакунка «{ $name }». Назви опублікованих пакунків глобально унікальні, тому Smudgy не завантажив і не змінив цей пакунок.
package-preparing-cache = Завантаження та перевірка файлів пакунка…
package-required-root-version-invalid = Smudgy не може перевірити вимоги пакунка { $name }, оскільки його версія недійсна: { $error }
package-required-graph-unstable = Граф потрібних пакунків не має одного стабільного розв'язання версій. Встановлення зупинено.
package-source-nul-warning = Цей вихідний код містить байти NUL. Smudgy показує кожен із них як ␀, щоб не приховувати виконуваний текст. Цей режим перевірки доступний лише для читання.
automation-publication-record-warning = Цей номер версії вже використано. Не публікуйте його повторно. Перевірте цю інформацію:
    { $warnings }
automation-publication-response-lost-warning = Smudgy не отримав остаточну відповідь. Smudgy перевірив, що на сервері є точний вміст { $name }@{ $version }. Не публікуйте цю версію повторно.
automation-publication-inconsistent-response-warning = Сервер повернув неузгоджену остаточну відповідь. Smudgy незалежно перевірив точний вміст { $name }@{ $version }. Не публікуйте цю версію повторно.
automation-publication-existing-version-warning = Smudgy знайшов на сервері точний вміст { $name }@{ $version }. Попередня публікація завершилася. Не публікуйте цю версію повторно.
automation-publication-link-missing-warning = { $name }@{ $version } є на сервері, але локальне посилання на публікацію відсутнє. Перезавантажте список пакунків, щоб відновити посилання.
automation-publication-link-unverified-warning = { $name }@{ $version } є на сервері, але Smudgy не зміг перевірити локальне посилання на публікацію: { $error }. Перезавантажте список пакунків перед перейменуванням або видаленням локального пакунка.
automation-publication-local-changed-warning = { $name }@{ $version } є на сервері, але локальний пакунок змінився під час публікації. Опублікована версія містить попередні файли. Перевірте опублікований вихідний код перед створенням наступної версії.
automation-publication-description-warning = { $name }@{ $version } є на сервері, але Smudgy не зміг оновити опис пакунка: { $error }. Ви можете опублікувати пізнішу версію, щоб повторити спробу оновлення опису.
package-shared-check = ✓ Доступ надано
package-local-override-note = Цей локальний пакунок має пріоритет над установленим пакунком із такою самою назвою. Установлений пакунок повернеться після видалення локального пакунка.
package-local-reconcile-failed = Smudgy не вдалося підготувати налаштування локального пакунка: { $error }
package-local-state-unavailable = Smudgy не може прочитати повний стан локальних пакунків. Останній коректний стан залишається видимим, але зміни вимкнено: { $error }
package-installed-state-unavailable = Smudgy не може прочитати стан установлених пакунків. Останній коректний стан залишається видимим, але зміни вимкнено: { $error }
package-settings-read-unavailable = Smudgy не може прочитати налаштування цього пакунка. Жодні значення не змінено: { $error }
package-settings-read-unavailable-generic = Smudgy не може прочитати налаштування цього пакунка. Перезавантажте дані та спробуйте ще раз.
package-settings-row-missing = Smudgy не може знайти запис налаштувань цього пакунка.
package-param-prompt-scope-changed = Цей пакунок тепер має окремі налаштування для кожного профілю. Закрийте це вікно й налаштуйте кожен активний профіль у вкладці «Налаштування».

# ---- Alias and trigger matcher editors ----
editor-match-input-as = Зіставляти введення як
editor-kind-command = Команда + аргументи
editor-kind-pattern = Простий шаблон
editor-kind-regex = Регулярний вираз
editor-kind-raw = Необроблені байти
editor-badge-advanced = Розширений
editor-badge-wizardry = Експертний
editor-card-example-command = greet <person>
editor-raw-hint = Запишіть \e як символ екранування. Коди кольорів і байти керування залишаються в рядку.
editor-group-exceptions = Винятки
editor-group-exceptions-note = Не дає цьому тригеру спрацювати
editor-group-raw = Необроблені збіги
editor-group-raw-note = Зіставляються з необробленим рядком разом із кодами кольорів
editor-add-raw-another = Додати ще один необроблений шаблон
editor-add-normal = Додати звичайний шаблон
editor-add-exception-another = Додати ще один виняток
editor-add-exception = Додати виняток
editor-add-exception-tip = Якщо є збіг, тригер не спрацює
editor-match-raw = Зіставляти необроблені байти
editor-match-raw-tip = Необроблений регулярний вираз, що зіставляється перед текстом зі збереженням кодів кольорів і байтів керування
editor-match-color = Зіставляти колір
editor-color-foreground = Передній план
editor-color-background = Тло
editor-color-any = Будь-який
editor-color-any-short = будь-який
editor-color-any-note = Цей канал не обмежує зіставлення.
editor-color-ansi = ANSI
editor-color-xterm = Xterm 256
editor-color-selected = Вибрано: { $color }
editor-color-truecolor = Truecolor
editor-color-range = Діапазон кольорів
editor-color-preview = Попередній перегляд
editor-color-from = Від
editor-color-to = До
editor-color-truecolor-note = Введіть один точний колір як шістнадцятковий код або значення RGB.
editor-color-range-note = Відтінок (H) іде від «Від» до «До» в напрямку зростання градусів і може проходити через 0°; однакові значення вибирають один відтінок. Насиченість (S) і яскравість (V) мають включні межі в будь-якому порядку. Діапазони зіставляються лише з кольорами на основі RGB: truecolor і xterm 16–255, але не ANSI чи xterm 0–15.
editor-color-hex = Hex
editor-color-red = R
editor-color-green = G
editor-color-blue = B
editor-color-invalid-hex = Введіть шестизначний шістнадцятковий код кольору, наприклад #1a2b3c.
editor-color-invalid-rgb = У кожному полі RGB введіть ціле число від 0 до 255.
editor-color-needs-constraint = Для зіставлення без тексту виберіть колір переднього плану або тла чи вимагайте атрибут тексту.
editor-color-attributes = Обов'язкові атрибути
editor-color-bold = Жирний
editor-color-faint = Тьмяний
editor-color-italic = Курсив
editor-color-underline = Підкреслення
editor-color-double-underline = Подвійне підкреслення
editor-color-slow-blink = Повільне блимання
editor-color-fast-blink = Швидке блимання
editor-color-crossed-out = Закреслення
editor-color-reverse = Інверсія
editor-move-up = Перемістити вгору
editor-move-down = Перемістити вниз
editor-remove-line = Вилучити цей рядок
editor-dot-matches = відповідає тестовому рядку
editor-dot-no-match = не відповідає тестовому рядку
editor-dot-blocks = відповідає — блокує тригер
editor-command = Команда
editor-arguments = Аргументи
editor-usage = Використання
editor-parsing = Розбір
editor-regex = Регулярний вираз
editor-arg-required = Обов'язковий
editor-arg-optional = Необов'язковий
editor-arg-rest = Решта рядка
editor-add-argument = Додати аргумент
editor-cmd-simple = Простий
editor-cmd-advanced = Розширений
editor-parse-spaces = Лише пробіли
editor-parse-quotes = Пробіли або лапки
editor-parse-braces = Пробіли або фігурні дужки
editor-parse-all = Пробіли, лапки або фігурні дужки
editor-parse-raw = Увесь рядок як один аргумент
editor-gets = ОТРИМУЄ
editor-parse-spaces-example = greet big "ugly" troll
editor-parse-spaces-gets = big · "ugly" · troll
editor-parse-quotes-example = greet "big ugly" troll
editor-parse-quotes-gets = big ugly · troll
editor-parse-braces-example = greet {"{"}big "ugly"{"}"} troll
editor-parse-braces-gets = big "ugly" · troll
editor-parse-all-example = greet "big ugly" {"{"}a "gift"{"}"}
editor-parse-all-gets = big ugly · a "gift"
editor-parse-raw-example = greet big ugly troll
editor-parse-raw-gets = big ugly troll
editor-allow-before = Дозволити текст перед шаблоном
editor-allow-after = Дозволити текст після шаблону
editor-prompt = Також зіставляти рядок запрошення
editor-prompt-note = Рядок, який гра залишає в очікуванні введення.
editor-syntax-pattern = Простий шаблон
editor-syntax-regex = Регулярний вираз
editor-command-name-empty = Назву команди ще не задано
editor-command-name-spaces = Оскільки цей аліас є командою, його назву потрібно скоротити до одного слова
editor-command-completion-note = Назви команд можна доповнювати клавішею Tab у полі введення.
editor-example-arg-name = name
editor-example-trigger-pattern = {"{"}person{"}"} says '{"{"}message{"}"}'
editor-example-trigger-regex = ^You are (hungry|thirsty)\.$
editor-example-trigger-raw = \e\[1;31m(?<hp>\d+)hp
editor-example-alias-simple = greet {"{"}person{"}"} warmly
editor-example-alias-regex = ^greet\s+(.+?)[!.]?$
editor-numbered-hole = Запишіть {"{}"} замість {"{"}{ $body }{"}"}. Поля нумеруються в порядку запису.
editor-unknown-hole-type = Невідомий тип у {"{"}{ $body }{"}"}. Використовуйте :word, :number або :rest.
editor-invalid-regex = Некоректний регулярний вираз: { $error }
editor-matches-every-line = Цей шаблон відповідає кожному рядку.
editor-fires-on-raw = ✓ спрацьовує за необробленим збігом { $n }
editor-fires-on-match = ✓ спрацьовує за збігом { $n }
editor-blocked-by = ✗ заблоковано винятком { $n }
editor-wrong-first-word = ✗ перше слово — не «{ $name }»
editor-missing-arg = ✗ бракує <{ $name }>
editor-unclaimed = ✗ жоден елемент не захоплює «{ $text }»
editor-unterminated-quote = ✗ незакриті лапки
editor-unbalanced-braces = ✗ незбалансовані фігурні дужки
editor-row-error = Рядок { $row }: { $error }
editor-reveal-order-aliases = Налаштувати пріоритет або заборонити спрацьовування інших аліасів
editor-reveal-order-triggers = Налаштувати пріоритет або заборонити спрацьовування інших тригерів
editor-hide-order = Сховати параметри пріоритету
# Inner triggers: triggers inside other triggers
editor-kind-inside = { $kind } · всередині { $outer }
editor-inside = Всередині
editor-inside-reach-line = лише цей рядок
editor-inside-reach-lines = у межах { $count } рядків
editor-inside-reach-unlimited = без обмеження рядків
editor-inside-reach-prompt = до запрошення
editor-inside-reach-once = один раз
editor-add-inside = Додати тригер всередині цього
editor-move-inside = Перемістити в інший тригер…
editor-inside-picker = Всередині
editor-no-trigger = (жодного)
editor-no-limit = без обмеження
editor-within = У межах
editor-lines-after = рядків після
editor-may-same-line = Може спрацювати в тому ж рядку, що й { $outer }
editor-may-repeat = Може спрацювати більше одного разу в цьому діапазоні
editor-may-after-prompt = Може спрацювати після появи запрошення
editor-match-outer-values = Зіставляти зі значеннями, які захопив { $outer }, а не з усім рядком
editor-never-fires = Цей тригер ніколи не спрацює. Дозвольте той самий рядок або задайте діапазон.
editor-overlap = Якщо { $outer } спрацює знову під час спостереження
editor-overlap-restart = Почати спочатку
editor-overlap-each = Спостерігати за кожним окремо
editor-verdict-inside-suffix = всередині { $outer }
editor-folder-from-outer = Тека · { $folder } (від { $outer })
editor-delete-outer-question = Видалити цей тригер і { $count ->
        [one] { $count } тригер усередині нього
        [few] { $count } тригери всередині нього
        [many] { $count } тригерів усередині нього
       *[other] { $count } тригера всередині нього
    }?
editor-move-inside-out = Перемістити їх назовні
editor-delete-inside-too = Видалити і їх
palette-move-inside = Перемістити { $subject } всередину { $outer }
palette-move-outside = Перемістити { $subject } з { $outer }

editor-matched-values = Зіставлені значення

# Try-it and test results
editor-try-alias-cta = Спробуйте з текстом, який ви могли б ввести
editor-try-trigger-cta = Спробуйте з рядком, надісланим грою
editor-try-it = Спробувати
editor-game-sent = ГРА НАДІСЛАЛА
editor-test-placeholder-alias = greet Mira
editor-test-placeholder-trigger = Mira says 'Follow me!'
editor-try-bytes-prefix = як байти ·
editor-verdict-no-command = ✗ назву команди ще не задано
editor-verdict-command-spaces = ✗ команда — це одне слово, тому збіг неможливий
editor-verdict-no-pattern = ✗ шаблон ще не задано
editor-verdict-no-regex = ✗ регулярний вираз ще не задано
editor-verdict-no-matchers = ✗ немає рядків для зіставлення
editor-verdict-invalid-regex = ✗ некоректний регулярний вираз: { $error }
editor-verdict-compile-error = ✗ { $error }

# When it runs
editor-when-it-runs = Коли спрацьовує
editor-priority = Пріоритет
editor-priority-note-aliases = Аліаси з вищими номерами спрацьовують першими.
editor-priority-note-triggers = Тригери з вищими номерами спрацьовують першими.
editor-continue-aliases = Також дозволити спрацювати іншим відповідним аліасам
editor-continue-triggers = Також дозволити спрацювати іншим відповідним тригерам
editor-allow-self-match = Дозволити тексту, який надсилає цей аліас, збігатися з ним самим

# What it reads (exposed state)
editor-reveal-state = Зчитувати стан із GMCP або пакунка
editor-hide-state = Сховати параметри стану
editor-what-it-reads = Що зчитує
editor-state-values = Значення стану
editor-state-exposed = Відкрито
editor-state-browse = Огляд
editor-state-filter-placeholder = Фільтрувати шляхи
editor-state-add-path = Додати шлях
editor-state-add-reveal = Вручну відкрити шлях стану (розширено)
editor-state-handle = Дескриптор
editor-state-remove = Вилучити
editor-state-rename = Використати іншу назву…
editor-state-name = Назва
editor-state-expose-tooltip = Відкрити { $path }
editor-state-unexpose-tooltip = Припинити відкривати { $path }
editor-state-insert-tooltip = Вставити { $reference }
editor-state-not-exposed = { $reference } не відкрито для цього елемента ({ $kind })
editor-state-name-taken = Інше відкрите значення вже використовує назву { $name }
editor-state-name-shadows-smudgy = { $name } зі Smudgy буде недоступним у цьому скрипті.
editor-state-name-shadows-javascript = { $name } із JavaScript буде недоступним у цьому скрипті.
editor-state-name-shadows-deno = { $name } із Deno буде недоступним у цьому скрипті.
editor-state-name-shadows-capture = { $name } також є зіставленим значенням; саме лише ${ $name } означає збіг
editor-state-bad-name = { $name } не є припустимою назвою
editor-state-reserved-name = { $name } — ключове слово JavaScript, тому не може бути назвою
editor-state-bad-path = { $path } не є припустимим шляхом
editor-state-empty = Сховище наразі порожнє.
editor-state-invalid = Не вдається використати відкрите значення: { $error }

# Action module
editor-tab-send-text = Надіслати текст
editor-tab-run-js = Запустити JavaScript
editor-gen-alias-hello = say Hello, { $hole }!
editor-gen-alias-hello-none = say Hello!
editor-gen-alias-emote = emote smiles and waves to { $hole }.
editor-gen-alias-emote-none = emote smiles and waves.
editor-gen-trigger = say I heard about { $hole }.
editor-gen-trigger-none = say Understood.

# Field hints
editor-gutter-before-pattern = Перед шаблоном може бути будь-який текст
editor-gutter-after-pattern = Після шаблону може бути будь-який текст
editor-gutter-before-regex = Немає ^ — перед виразом може бути будь-який текст
editor-gutter-after-regex = Немає $ — після виразу може бути будь-який текст

# Header descriptions and footer links
editor-deck-alias = Аліаси дають змогу створювати власні команди.
editor-deck-trigger = Тригери реагують на текст, надісланий грою.
editor-delete-this-alias = Видалити цей аліас
editor-delete-this-trigger = Видалити цей тригер
widget-hotkey-click-to-record = Натисніть, щоб зареєструватися
widget-hotkey-listening = очікування клавіш…

# Package manifest editor
manifest-kind-text = Текст
manifest-kind-boolean = Булеве значення
manifest-kind-number = Число
manifest-kind-dropdown = Розкривний список
manifest-kind-list = Список
manifest-kind-table = Таблиця
manifest-version-required = Потрібно вказати версію (напр., 1.0.0).
manifest-min-version-invalid = Значення поля «Потребує Smudgy» — «{ $version }» — не є версією (напр., 0.4.0).
manifest-param-needs-key = Параметр #{ $number } потребує ключа.
manifest-param-error = Параметр «{ $key }»: { $reason }
manifest-duplicate-param-key = Дубльований ключ параметра «{ $key }».
manifest-dropdown-needs-option = Розкривний список потребує щонайменше однієї опції.
manifest-list-needs-element = Список потребує типу елемента.
manifest-list-element = елемент списку
manifest-column = стовпець «{ $key }»
manifest-duplicate-column = Дубльований ключ стовпця «{ $key }».
manifest-table-needs-column = Таблиця потребує щонайменше одного стовпця з ключем.
manifest-no-nested-container = { $item } не може саме бути списком чи таблицею.
manifest-item-dropdown-needs-option = { $item } — розкривний список, тож потрібна щонайменше одна опція.
manifest-duplicate-option = Дубльоване значення опції «{ $value }».
manifest-default-from-options = Типове значення має бути однією з опцій.
manifest-default-boolean-error = Типове значення має бути true або false.
manifest-default-number-error = Типове значення має бути числом.
manifest-serialize-failed = Не вдалося серіалізувати маніфест: { $error }
manifest-save-failed = Не вдалося зберегти: { $error }
manifest-changed-outside = Маніфест змінено поза Smudgy. Скасуйте це редагування, відкрийте його знову та перевірте зміни перед збереженням.
manifest-saved = Маніфест збережено.
manifest-edit = Редагувати маніфест
manifest-edit-help = Структурований редактор файлу smudgy.package.json.
manifest-version = Версія
manifest-version-placeholder = напр., 1.0.0
manifest-version-warning = Наразі це некоректна версія semver — обов'язкова перед публікацією (напр., 1.2.3).
manifest-description = Опис
manifest-runtime-compatibility = Сумісність із середовищем виконання
manifest-target-native = Настільний застосунок
manifest-target-web = Вебверсія
manifest-target-both = Обидві версії
manifest-description-placeholder = Що робить цей пакунок (показується в розділі Огляд)
manifest-entry = Точка входу
manifest-entry-placeholder = index.ts
manifest-unsaved = Незбережені зміни
manifest-save = Зберегти маніфест
manifest-title = Маніфест
manifest-auto-entry = index.* (розпізнається автоматично)
manifest-no-description = Немає опису
manifest-requires-smudgy = Потребує Smudgy
manifest-any-version = Будь-яка версія
manifest-aligned-hosts = Відповідні хости
manifest-any-host = Будь-який хост
manifest-dependencies = Залежності
manifest-parameters = Параметри
manifest-permissions = Дозволи
manifest-connections = З'єднання
manifest-code-imports = Імпорти коду
manifest-read-files = Читання файлів
manifest-write-files = Запис файлів
manifest-environment = Середовище
manifest-capabilities = Можливості
manifest-system-info = Системна інформація
manifest-run-programs = Запуск програм
manifest-native-libraries = Нативні бібліотеки
manifest-none = Немає
manifest-none-period = Немає.
manifest-required = обов'язковий
manifest-secret = таємний
manifest-default-value = типово { $value }
manifest-kind-dropdown-summary = { $count ->
        [one] розкривний список ({ $count } опція)
        [few] розкривний список ({ $count } опції)
        [many] розкривний список ({ $count } опцій)
       *[other] розкривний список ({ $count } опції)
    }
manifest-kind-list-summary = список типу { $kind }
manifest-kind-table-summary = таблиця [{ $columns }]
manifest-fully-sandboxed = Повністю в пісочниці — без спеціальних можливостей.
manifest-cap-summary = { $api } — { $gloss }
manifest-params-help = Значення, що налаштовуються під час встановлення. Обов'язкові блокують завантаження, доки їх не задано; таємні потрапляють до системного сховища ключів.
manifest-no-parameters = Немає параметрів.
manifest-add-parameter = Додати параметр
manifest-param-key-placeholder = ключ (напр., apiToken)
manifest-param-label-placeholder = підпис, що показується користувачам (необов'язково)
manifest-options = Опції
manifest-each-entry-is = Кожен запис —
manifest-columns = Стовпці
manifest-columns-help = Кожен рядок зберігає одне значення на стовпець, індексоване ключем стовпця.
manifest-add-column = Додати стовпець
manifest-default-bool-placeholder = типово: true або false
manifest-default-number-placeholder = типове число (необов'язково)
manifest-default-text-placeholder = типовий текст (необов'язково)
manifest-no-options = Немає опцій.
manifest-option-value-placeholder = значення (зберігається)
manifest-option-label-placeholder = підпис (необов'язково)
manifest-add-option = Додати опцію
manifest-default = Типове значення
manifest-no-default = (немає типового значення)
manifest-column-key-placeholder = ключ стовпця
manifest-value-of-type = значення типу
manifest-tab-settings = Налаштування
manifest-tab-capabilities = Можливості
manifest-tab-network = Мережа
manifest-tab-files = Файли
manifest-tab-system = Система
manifest-sandbox-deny-note = Встановлення в пісочниці не має доступу ні до чого, що не перелічено тут.
manifest-dependency-lock-note = Публікація фіксує точну версію кожної залежності на момент публікації. Користувачі отримають новіші версії залежностей лише після того, як ви випустите нову версію цього пакунка, яка їх оновить.
manifest-readable-path-warning = Шлях для читання поза $DATA спричиняє попередження. Надавайте перевагу $DATA, якщо читання зовнішніх файлів не є необхідним.
manifest-dependencies-help = Інші пакунки Smudgy, які імпортує цей пакунок: smudgy:@name@^1.2 (також працює smudgy://owner/name@^1.2). Наразі підтримуються лише пакунки Smudgy. Версіями пакунків jsr і npm керують їхні завантажувачі, і пакунок може їх імпортувати, коли має доступ до реєстру.
manifest-add-dependency-placeholder = Додайте один зі своїх встановлених або локальних пакунків…
manifest-dependency = залежність
manifest-any-version-placeholder = будь-яка версія
manifest-min-version-help = Мінімальна версія Smudgy, на якій працює цей пакунок. Це може запобігти автоматичному оновленню старіших клієнтів до несумісної версії пакунка. Залиште порожнім, щоб дозволити будь-яку версію Smudgy.
manifest-min-version-format-error = Це не версія. Використайте формат semver, напр., 0.3.5, або залиште порожнім.
manifest-min-version-newer = Новіша за цю версію Smudgy ({ $running }) — встановлення й завантаження відхиляються нижче за { $required } (ваша локальна копія розробника залишається поза цим правилом).
manifest-hosts-help = Хости, на які націлений цей пакунок у розділі Огляд, застосовуються під час публікації. Залиште порожнім, щоб він був незалежним від хоста.
manifest-host = хост
manifest-required-packages = Обов'язкові пакунки
manifest-required-packages-help = Пакунки, що встановлюються автоматично разом із цим пакунком, кожен працює у власній пісочниці. Використайте smudgy:@name[@^1.2].
manifest-required-package = обов'язковий пакунок
manifest-allow-import = Дозволити іншим імпортувати цей пакунок
manifest-allow-import-help = Коли вимкнено, модулі цього пакунка можуть імпортувати лише ваші пакунки. Інші пакунки отримують лише типи. Ця поведінка може змінитися в майбутній версії.
manifest-allowed-hosts = Дозволені хости
manifest-host-format-help = ім'я хоста, ім'я хоста:порт, * (будь-який хост/порт) або *:порт (будь-який хост на цьому порту).
manifest-local-ipc = Локальний IPC
manifest-local-ipc-help = Локальні служби, до яких цей пакунок може підключитися. Кожен рядок — одна кінцева точка, оголошена для обох родин платформ: шлях Unix-сокета та/або ім'я каналу Windows (Smudgy сам додає \\.\pipe\). Надається лише поле, що відповідає платформі користувача.
manifest-ipc-unix-label = Шлях Unix-сокета
manifest-ipc-pipe-label = Ім'я каналу Windows
manifest-ipc-unix-placeholder = /var/run/example.sock
manifest-ipc-pipe-placeholder = example-service
manifest-ipc-endpoint = локальну кінцеву точку IPC
manifest-ipc-warning = Локальна кінцева точка IPC може діставатися привілейованих системних служб (наприклад, сокета Docker) з повними правами користувача.
manifest-ipc-unix-invalid = Рядок Локального IPC №{ $number }: шлях Unix-сокета має бути абсолютним (починатися з /).
manifest-ipc-pipe-invalid = Рядок Локального IPC №{ $number }: ім'я каналу Windows не може містити /, \ чи : — Smudgy сам додає \\.\pipe\.
manifest-net-local-transport-error = Дозволений хост «{ $entry }» називає локальний сокет. Дозволені хости — лише інтернет-хости; локальні кінцеві точки оголошуються в розділі «Локальний IPC».
manifest-pipe-namespace-path-error = Шлях «{ $entry }» вказує на простір імен каналів Windows. Відкрити канал означає підключитися до нього; оголосіть цю кінцеву точку в розділі «Локальний IPC».
manifest-import-none = Немає модулів поза екосистемою Smudgy
manifest-import-registries = Може завантажувати й запускати модулі з публічних реєстрів (npm, jsr)
manifest-import-any = Може завантажувати й запускати модулі з будь-якого джерела
manifest-import-registries-summary = Публічні реєстри (npm, jsr)
manifest-import-any-summary = Будь-де в мережі
manifest-readable-paths = Шляхи для читання
manifest-data-dir-help = $DATA — це каталог даних пакунка
manifest-writable-paths = Шляхи для запису
manifest-path = шлях
manifest-readable-env = Змінні середовища для читання
manifest-env-help = Точні імена змінних, які він може прочитати.
manifest-variable = змінна
manifest-add-item = Додати: { $item }
manifest-capabilities-help = API smudgy:core, які можуть викликати ваші скрипти (блок permissions.smudgy). Виклик API, про який не було запиту, спричиняє помилку під час виконання.
manifest-cap-group-automations = Автоматизації
manifest-cap-group-session = Сесія
manifest-cap-group-display = Відображення
manifest-cap-group-mapper = Мапер
manifest-cap-group-widgets = Віджети
manifest-cap-group-interop = Interop
manifest-cap-group-panes = Панелі
manifest-cap-group-gmcp = GMCP
manifest-cap-group-workers = Воркери
manifest-cap-create-aliases = визначення вхідних аліасів
manifest-cap-create-triggers = реагування на вихідні дані гри
manifest-cap-send = надсилання команд так, ніби їх було введено (проходять через ваші аліаси)
manifest-cap-send-raw = надсилати текст і необроблені байти безпосередньо до гри, оминаючи аліаси. Smudgy не показує необроблені байти у вікні виводу
manifest-cap-echo = показ тексту на вашому екрані
manifest-cap-sessions = доступ до інших ваших підключених сесій
manifest-cap-display = приховування, виділення, вставлення або заміна тексту
manifest-cap-mapper-read = читання ваших мап
manifest-cap-mapper-read-required = читання ваших мап (потрібне для зміни мап)
manifest-cap-mapper-write = зміна ваших мап
manifest-cap-widgets = створення й зміна віджетів на екрані
manifest-cap-interop-write = розсилання подій і публікація спільного стану, на який можуть реагувати інші пакунки
manifest-cap-interop-read = прослуховування подій і читання спільного стану
manifest-cap-interop-broadcast = використання BroadcastChannel з пакунками в сесіях на цьому сервері
manifest-cap-panes = створення розділених панелей і взаємодія з ними
manifest-cap-gmcp = надсилання повідомлень GMCP до гри та керування модулями GMCP
manifest-cap-workers = запуск фонових обчислювальних потоків (без доступу до мережі, файлів чи smudgy)
param-value-not-choice = «{ $value }» не є однією з доступних опцій.
param-value-number-error = Має бути числом.
param-value-default = типово: { $value }
param-value-number = число
param-value-value = значення
param-value-none = (немає)
param-value-no-entries = Немає записів.
param-value-no-element = (тип елемента не оголошено)
param-value-add-entry = Додати запис
param-value-no-columns = Стовпці не оголошено.
param-value-no-rows = Немає рядків.
param-value-add-row = Додати рядок
automation-published = Опубліковано v{ $version }
automation-published-typings = типізації: { $count }
automation-typings-warning = ⚠ типізації: { $warnings }
automation-locked-dependencies = залежності: { $dependencies }
automation-interop-warning = ⚠ interop: { $warnings }
automation-publish-failed = Не вдалося опублікувати: { $error }
automation-reloaded = Перезавантажено скрипти для { $server }.
automation-tab-unsaved = { $label } (не збережено)
automation-nav-unsaved = У вас є незбережені зміни.
automation-keep-editing = Продовжити редагування
automation-code-intelligence-not-applicable = Аналіз коду не застосовується до цього файлу
automation-code-intelligence-starting = Запуск аналізу коду…
automation-code-intelligence-ready = Аналіз коду готовий
automation-code-intelligence-unavailable = Аналіз коду недоступний · редагування залишається доступним
automation-code-problems = Проблеми
automation-code-completions = Доповнення
automation-code-go-to-definition = Перейти до визначення
automation-code-format-document = Форматувати документ
automation-code-show-completions = Показати пропозиції

# Package management
package-sign-in-shared = Увійдіть у головному вікні в Налаштування → Обліковий запис, щоб побачити пакунки, якими ви володієте, а також ті, що надані друзями.
package-no-selection = Пакунок не вибрано.
package-dependency-managed = Працює всередині { $packages }.
package-direct-and-required = Його також потребує { $packages }.
package-disabled-review = Вимкнено. Увімкніть у Налаштуваннях, коли довірятимете йому.
package-metric-author = Автор
package-metric-loaded = Завантажена
package-metric-latest-blocked = Найновіша (заблокована)
package-metric-update = Оновлення
package-update-auto = Авто
package-update-pinned = Закріплена
package-metric-installs = Встановлення
package-required-by = Потрібен для
package-needs = потребує
package-update-mode = Режим оновлення
package-update-auto-track = Авто — стежити за найновішою
package-update-pinned-placeholder = Закріплена — виберіть версію…
package-dependencies = Залежності
package-dependency-auto-remove = Ця залежність вилучається автоматично, коли жоден встановлений пакунок її більше не потребує.
package-state-active = активний
package-state-inactive = неактивний
package-state-enabled = увімкнений
package-state-disabled = вимкнений
package-tab-readme = README
package-tab-source = Джерело
package-no-readme = Немає README.
package-readme-loading = Завантаження README…
package-readme-load-failed = Не вдалося завантажити README: { $error }
package-loading = Завантаження…
package-no-source-files = Цей пакунок не містить жодних вихідних файлів.
package-select-source = Виберіть файл, щоб переглянути його джерело.
package-source-missing = Цей файл більше не є частиною пакунка.
package-source-fetching = Завантаження джерела…
package-source-bidi-warning = Увага: цей файл містить двонапрямлені або невидимі символи керування, тож показаний текст може не відповідати тому, що насправді виконується.
package-source-binary = Двійковий файл ({ $size }) — не показано.
package-source-too-large = Файл має { $size } — завеликий для перегляду (ліміт { $limit }).
package-source-load-error = Не вдалося завантажити джерело: { $error }
    Виберіть файл ще раз, щоб спробувати знову.
package-open-own-pane = Відкрити його власну панель
package-actions = Дії
package-edit-copy = Редагувати копію
package-edit-copy-help = Збережіть назву, щоб замінити цей пакунок локальною копією. Вкажіть нову назву, щоб створити окремий пакунок.
package-copy-name = Локальна назва
package-create-copy = Створити копію
package-open-local-copy = Відкрити локальний пакунок
package-copy-name-exists = Локальний пакунок із назвою «{ $name }» уже існує. Відкрийте його або вкажіть іншу назву.
package-copy-requirements-not-ready = Smudgy не створив копію, оскільки спочатку потрібно встановити або оновити один чи кілька обов'язкових пакунків. Встановіть або оновіть їх, а потім повторіть спробу.
package-remove-standalone = Вилучити окреме встановлення
package-remove-standalone-help = { $name } залишається встановленим для { $packages }.
package-removal-required = Вилучення { $name } також вилучить { $count ->
        [one] { $count } пакунок, який його потребує
        [few] { $count } пакунки, які його потребують
        [many] { $count } пакунків, які його потребують
       *[other] { $count } пакунка, який його потребує
    }: { $packages }.
package-remove-orphans = Вилучити також { $packages }? Ніщо інше їх не потребує.
package-disable-cascade-warning = Вимкнення { $name } у { $profiles } також вимкне { $count ->
        [one] { $count } пакунок, якому він потрібен
        [few] { $count } пакунки, яким він потрібен
        [many] { $count } пакунків, яким він потрібен
       *[other] { $count } пакунка, якому він потрібен
    }: { $packages }. Вимкнути їх разом?
package-disable-cascade-confirm = Вимкнути всі
package-disable-cascade-applied = Вимкнено { $name } та { $packages }.
package-enabling-starts-required = Увімкнення { $name } також запустило { $packages }.
package-remove-all = Вилучити все
package-remove = Вилучити
package-uninstall = Деінсталювати
package-remove-together-question = Вилучити їх разом?
package-remove-standalone-question = Вилучити окреме встановлення?
package-uninstall-question = Видалити пакунок? Налаштування залишаться, секрети буде видалено.
package-keep-orphans = Зберегти їх
package-remove-standalone-ellipsis = Вилучити окреме встановлення…
package-uninstall-name = Деінсталювати { $name }…
package-kind-alias = аліас
package-kind-trigger = тригер
package-kind-hotkey = гаряча клавіша
package-automation-unavailable = { $name } більше недоступний.
package-creator-module = модуль { $name }
package-creator-package = пакунок { $name }
package-readonly-created-by = { $kind } лише для читання · створено { $creator }
package-created-managed = Створено й керується його пакунком або модулем — тут його не можна редагувати чи перемикати.
package-open-creator = Відкрити { $creator }
package-pattern = Шаблон
package-none-parenthetical = (немає)
package-body-sends = Надсилає
package-body-script = Скрипт
package-body-script-unavailable = (обробник JavaScript — джерело недоступне)
package-body-does = Виконує
package-body-nothing = (нічого)
package-public = Публічний
package-private = Приватний
package-owned-subtitle = Ви власник цього пакунка · v{ $version }
package-new-name-placeholder = нова назва
package-save-name = Зберегти назву
package-rename = Перейменувати
package-enabled = Увімкнений
package-metric-latest = Найновіша
package-metric-versions = Версії
package-publish = Опублікувати
package-publish-output = Результат публікації
package-save-before-publish = Збережіть зміни у вихідному коді та маніфесті перед публікацією.
package-publish-sign-in = Увійдіть, перш ніж публікувати цей пакунок.
package-account-changed = Обліковий запис змінився. Перевірте пакунок, а потім опублікуйте його знову.
package-account-review-changed = Обліковий запис змінився. Перевірте цей пакунок, а потім повторіть спробу.
package-operation-in-progress = Виконується інша зміна цього пакунка. Зачекайте, доки вона завершиться, і повторіть спробу.
manifest-operation-expired = Ця зміна маніфесту більше не активна. Закрийте цей перегляд, а потім збережіть маніфест ще раз.
package-save-before-rename = Збережіть зміни у вихідному коді та маніфесті перед перейменуванням пакунка.
package-version-already-used = Версію v{ $version } вже опубліковано. Номери версій не можна використовувати повторно.
package-version-up-to-date = Версію v{ $version } опубліковано, і вона містить усі локальні зміни.
package-version-local-changes = У вас є локальні зміни, внесені після публікації версії v{ $version }. Опубліковані версії не можна оновлювати. Збільште номер версії, щоб опублікувати зміни.
package-version-checking = Перевірка локальних змін після версії v{ $version }…
package-increase-version = Збільшити номер версії
package-published-versions = Опубліковані версії
package-no-published-versions = Немає опублікованих версій.
package-version-deleted = видалена
package-version-latest = найновіша
package-version-yanked = відкликана
package-version-unyank = Відновити
package-version-yank = Відкликати
package-yank-help = Відкликання запобігає новим встановленням, не видаляючи пакунок примусово з наявних встановлень.
package-delete-question = Видалити цей пакунок і всі його файли?
package-delete-ellipsis = Видалити пакунок…
package-show-explorer = Показати в Провіднику
package-show-finder = Показати у Finder
package-open-folder = Відкрити теку
package-select-file-edit = Виберіть файл для редагування.
package-sharing = Спільний доступ
package-publish-before-sharing = Спершу опублікуйте цей пакунок, а потім надайте до нього доступ.
package-public-help = Будь-хто може його знайти та встановити.
package-private-help = Встановити його можуть лише друзі, яким ви надасте доступ.
package-make-private = Зробити приватним
package-make-public = Зробити публічним
package-no-friends = Друзів не знайдено.
package-unknown-user = невідомий
package-shared = ✓ Доступ надано
package-share = Надати доступ
package-new = Новий пакунок
package-new-subtitle = Створіть пакунок smudgy:// для надання доступу
package-name = Назва
package-name-placeholder = напр., mySpellTriggers
package-new-help = Пакунок — це невелика програма. Може містити аліаси, тригери, гарячі клавіші, модулі та ресурси, якими можуть користуватися інші пакунки.
package-create = Створити пакунок
package-discover = Огляд пакунків
package-discover-subtitle = Переглядайте та встановлюйте публічні пакунки
package-search-placeholder = Пошук пакунків…
package-search = Шукати
package-scope = Область
package-scope-relevant = Відповідні
package-scope-host = Лише для { $host }
package-scope-universal = Лише універсальні пакунки
package-scope-all = Усі пакунки
package-working = Обробка…
package-no-results = Немає результатів — введіть запит і натисніть Шукати.
package-manage = Керувати
package-view = Переглянути
package-install = Встановити
package-upgrade-smudgy = Оновити Smudgy
package-installed = Встановлено
package-available-with-smudgy-upgrade = Пакунок v{ $package_version } доступний у Smudgy { $smudgy_version } або новішій версії.
package-search-meta = { $count ->
        [many] { $owner } · v{ $version } · { $count } встановлень ·
       *[other] { $owner } · v{ $version } · { $count } встановлення ·
    }
package-you = Ви
package-owner-clan = Клан
package-owner-me = Я
package-owner = Власник: { $owner }
package-publish-as = Опублікувати як
package-publish-as-clan-help = { $clan } стане його власником: кожен учасник клану зможе його встановити, а клан вирішує, хто публікує нові версії. Власника пакунка згодом змінити не можна.
package-clan-private-help = Встановити його можуть лише учасники клану.
package-clan-cannot-create = { $clan }: ви не можете створювати пакунки в цьому клані. Опублікуйте його як свій або зверніться до власників клану.
package-clan-cannot-publish = { $clan }: ви не можете публікувати нові версії цього пакунка.
package-clan-refused = { $clan }: ви не можете зробити це з цим пакунком.
package-claimed-for-me = Перервана публікація цього пакунка обрала власником вас. Опублікуйте його як свій, щоб завершити.
package-claimed-for-clan = Перервана публікація цього пакунка обрала власником { $clan }. Опублікуйте його в цьому клані, щоб завершити.
package-back = ‹ Назад
package-comments = Коментарі
package-comment-placeholder = Додати коментар…
package-post = Опублікувати
package-no-comments = Немає коментарів.
package-someone = хтось
package-configure = Налаштувати { $name } v{ $version }
package-required-settings-help = Обов'язкові налаштування (пакунок не завантажиться, доки їх не заповнено):
package-secret-placeholder = таємне значення
package-secret-stored-placeholder = задано — залиште порожнім, щоб зберегти
package-runtime-settings-help = Значення, які цей пакунок читає під час виконання. Обов'язкові мають бути задані, щоб він міг завантажитися.
package-saved = Збережено.
package-save-settings = Зберегти налаштування
package-install-title = Встановити { $name } v{ $version }
package-installing-compatible-version = Встановлюється сумісна версія v{ $current_version }. Пакунок v{ $package_version } стане доступним у Smudgy { $smudgy_version } або новішій версії.
package-update-review-title = Перевірити { $name } v{ $version }
package-manifest-review-title = Перевірити зміни в пакунку { $name }
package-local-publisher = Локальний пакунок
package-publisher = Видавець: { $publisher }
package-cannot-also = Також не може:
package-will-be-able = Цей пакунок зможе:
package-will-not-be-able = НЕ зможе:
package-note = Примітка:
package-install-conflict = Не вдалося встановити — конфлікт версій обов'язкового пакунка
package-install-newer-smudgy = Не вдалося встановити — потребує новішої версії Smudgy
package-message-period = { $message }.
package-install-disabled = Встановити без увімкнення
package-install-enabled = Встановити й увімкнути
package-also-installs = Це також встановлює:
package-already-installed-version = { $name } v{ $version } — уже встановлено
package-upgrade-version = { $name } → v{ $version } (оновлення)
package-name-version = { $name } v{ $version }
package-sandbox = Пісочниця
package-developing-unsandboxed = Розробка без пісочниці — повний доступ
package-unsandboxed-owned-help = Працює у вашому головному ізоляті з повним доступом до вашого комп'ютера, ніби він ваш. Поверніть його до пісочниці з маніфесту, щоб перевірити те, що отримують ті, хто встановлює.
package-use-manifest-sandbox = Використати пісочницю з маніфесту
package-runs-manifest-sandbox = Працює в пісочниці згідно з маніфестом цього пакунка
package-it-can = Може:
package-edit-capabilities = Редагувати можливості
package-develop-unsandboxed-question = Розробляти цей пакунок без пісочниці?
package-develop-unsandboxed-warning = Буде працювати з ПОВНИМ доступом до вашого комп'ютера, у вашому головному ізоляті, поділяючи стан з вашими власними скриптами та ігноруючи дозволи зі свого маніфесту.
package-develop-unsandboxed = Розробляти без пісочниці
package-develop-unsandboxed-advanced = Розробляти без пісочниці (розширене)
package-develop-unsandboxed-help = Запустіть його у своєму головному ізоляті з повним доступом і зневадженням в інспекторі, ігноруючи пісочницю з маніфесту.
package-develop-unsandboxed-ellipsis = Розробляти без пісочниці…
package-runs-inside = Працює всередині { $parent }
package-dependency-permissions-help = Це складова частина { $parent }. Працює в пісочниці { $parent } і може робити лише те, що дозволено { $parent }; не має власних дозволів. Щоб переглянути чи змінити його доступ, відкрийте { $parent }.
package-full-access = Повний доступ — пісочницю вилучено
package-effectively-full-access = Фактично повний доступ
package-effectively-full-access-reasons = Фактично повний доступ — може { $reasons }.
package-full-access-help = Працює у вашому головному ізоляті з повним доступом до вашого комп'ютера, ніби він ваш, поділяючи стан з вашими власними скриптами.
package-restore-sandbox = Відновити пісочницю
package-runs-sandbox = Працює в пісочниці
package-runs-sandbox-with-escape-grants = Працює в пісочниці — з дозволами, що дають змогу вийти за її межі
package-it-can-only = Може лише:
package-not-consented = Немає згоди — доступ заблоковано до підтвердження дозволів (перевстановіть із розділу Огляд).
package-remove-sandbox-question = Вилучити пісочницю з цього пакунка?
package-remove-sandbox-warning = Без пісочниці цей пакунок працює з ПОВНИМ доступом до вашого комп'ютера, у вашому головному ізоляті, поділяючи стан з вашими власними скриптами. Може зробити все те, що й ви. Вилучайте пісочницю лише для пакунків, які ви написали б самостійно.
package-remove-sandbox = Вилучити пісочницю
package-remove-sandbox-advanced = Вилучити пісочницю (розширене)
package-remove-sandbox-help = Запустіть його у своєму головному ізоляті з повним доступом до вашого комп'ютера. Це також уможливлює зневадження в інспекторі.
package-remove-sandbox-ellipsis = Вилучити пісочницю…
package-update-held-newer = Оновлення призупинено — { $name } потребує новішої версії Smudgy
package-version-held-reason = Версію v{ $version } призупинено — { $reason }.
package-ok = Гаразд
package-update-blocked-permissions = Оновлення заблоковано — { $name } потребує більше дозволів
package-update-current-held = Ви використовуєте версію v{ $current } (у межах вашої згоди). Версію v{ $next } призупинено — щоб її оновити, додатково потрібно:
package-update-held-load = Версію v{ $version } призупинено — щоб її завантажити, додатково потрібно:
package-keep-current-version = Зберегти поточну версію
package-grant-update = Надати й оновити
package-update-requirements-changed = Оновлення треба перевірити — { $name } змінює обов'язкові пакунки
package-update-requirements-help = Версія { $version } може додати, вилучити або оновити окремі програми пакунків. Перевірте їх перед оновленням.
package-review-update = Перевірити оновлення
package-apply-update = Застосувати оновлення

# Permission and consent descriptions
permission-import-registries = завантажувати й запускати код із публічних реєстрів (npm, jsr)
permission-import-anywhere = завантажувати й запускати код із будь-якого місця в мережі
permission-connect-to = з'єднуватися з
permission-connect-local-ipc = з'єднуватися з локальним IPC
permission-ipc-unix-socket = Unix-сокет { $path }
permission-ipc-windows-pipe = канал Windows { $name }
permission-ipc-foreign-platform = не використовується на цьому комп'ютері
permission-read = читати
permission-write = записувати
permission-read-env = читати змінну середовища
permission-can-create-aliases = Створювати аліаси
permission-can-create-triggers = Створювати тригери
permission-can-send-both = Надсилати команди через аліаси або надсилати текст і необроблені байти безпосередньо до гри. Smudgy не показує необроблені байти у вікні виводу. Гра все одно може надіслати відповідь.
permission-can-send-aliases = Надсилати команди до гри так, ніби ви їх ввели, потенційно спрацьовуючи аліаси
permission-can-send-direct = Надсилати текст і необроблені байти безпосередньо до гри, оминаючи аліаси. Smudgy не показує необроблені байти у вікні виводу. Гра все одно може надіслати відповідь.
permission-can-echo = Показувати текст на екрані
permission-can-sessions = Взаємодіяти з іншими відкритими сесіями, зокрема змінювати розташування вікон цього сервера
permission-can-display = Приховувати, змінювати стиль, вставляти або замінювати текст гри та бачити поточний рядок
permission-can-map-read = Читати ваші мапи
permission-can-map-write = Змінювати ваші мапи
permission-can-secrets-read = Читати Секрети та приватні доповнення ваших мап
permission-can-secrets-write = Змінювати Секрети та приватні доповнення ваших мап
permission-can-secrets-manage = Створювати, перейменовувати та видаляти Секрети ваших мап
permission-can-widgets = Створювати й змінювати віджети на екрані
permission-can-panes = Створювати панелі виводу сесії, спрямовувати до них рядки гри та зберігати й застосовувати іменовані розкладки вікон
permission-can-interop-write = Розсилати події пакунка й публікувати спільний стан, на який можуть реагувати інші пакунки
permission-can-interop-read = Прослуховувати події та читати спільний стан
permission-can-interop-broadcast = Використовувати BroadcastChannel з пакунками в сесіях на цьому сервері
permission-can-workers = Запускати фонові обчислювальні потоки (без доступу до мережі, файлів чи smudgy)
permission-can-gmcp = Надсилати повідомлення GMCP до гри та керувати модулями GMCP
permission-cannot-send = надсилати команди до гри
permission-cannot-aliases = створювати аліаси
permission-cannot-triggers = створювати тригери, що реагують на вихідні дані гри
permission-cannot-echo = показувати текст на екрані
permission-cannot-sessions = звертатися до інших ваших сесій
permission-cannot-display = взаємодіяти з терміналом гри (приховувати, стилізувати, вставляти чи замінювати текст; ані бачити поточний рядок)
permission-cannot-map-read = читати ваші мапи
permission-cannot-map-write = змінювати ваші мапи
permission-cannot-widgets = створювати віджети на екрані
permission-cannot-panes = створювати інші панелі чи отримувати до них доступ
permission-cannot-interop-write = розсилати події чи публікувати спільний стан
permission-cannot-interop-read = прослуховувати події чи читати спільний стан
permission-cannot-interop-broadcast = використовувати BroadcastChannel з пакунками в сесіях на цьому сервері
permission-cannot-workers = запускати фонові обчислювальні потоки
permission-cannot-gmcp = надсилати повідомлення GMCP до гри
permission-sandbox-summary = Працює в повній пісочниці — без доступу до ваших файлів, мережі чи системи.
permission-never-native = завантажувати нативний код чи запускати інші програми
permission-never-other-data = читати чи змінювати дані інших ваших пакунків або скриптів
permission-cannot-network = з'єднуватися з інтернетом чи будь-яким сервером
permission-cannot-network-except-import = з'єднуватися через мережу чи приймати з'єднання (усе ще може завантажувати код для запуску, як зазначено вище)
permission-cannot-import = завантажувати чи запускати код із npm, jsr чи з мережі
permission-cannot-read-files = читати ваші файли
permission-cannot-write-files = змінювати ваші файли
permission-cannot-read-env = читати змінні середовища
package-private-shared = Приватні та надані
package-private-shared-subtitle = Ваші пакунки, пакунки, надані друзями, та доступні вам пакунки кланів.
package-clan-packages = Пакунки кланів
package-your-packages = Ваші пакунки
package-local = Локальні
package-no-owned-cloud = Ви ще не володієте жодними пакунками у хмарі.
package-shared-with-you = Надані вам
package-no-shared = Вам ще не надано жодних пакунків.
package-owner-version = { $owner } · v{ $version }
package-unrated = без оцінки
package-rate = Оцінити
package-rating = Оцінка

# Package management feedback and errors
package-conflict-two-requirers = { $first } потребує { $name } { $first_range }, але { $last } потребує { $name } { $last_range }
package-conflict-one-requirer = Жодна опублікована версія { $name } не задовольняє { $range } (потрібна для { $requirer })
package-conflict-all-requirers = Жодна опублікована версія { $name } не задовольняє вимог усіх пакунків
package-enable-state-failed = Не вдалося оновити стан увімкнення: { $error }
package-disabled-name = Вимкнено { $name }.
package-disabled-with-deps = Вимкнено { $name } + { $dependencies } (більше не потрібні).
package-enabled-name = Увімкнено { $name }.
package-enabled-with-deps = Увімкнено { $name } + { $dependencies } (залежності).
package-switch-failed = Не вдалося перемкнути: { $error }
package-switched = Перемкнено на { $name }.
package-update-failed = Не вдалося оновити { $name }: { $error }
package-update-mode-failed = Не вдалося встановити режим оновлення: { $error }
package-uninstall-failed = Не вдалося деінсталювати: { $error }
package-partial-remove-failed = Вилучено { $removed }, але не вдалося вилучити { $failed }: { $error }
package-removed-standalone-toast = Вилучено окреме встановлення { $name }; залишається встановленим як залежність.
package-uninstalled-toast = Деінсталювано { $name }.
package-uninstalled-with = Деінсталювано { $name } + { $dependencies }.
package-none-selected = Пакунок не вибрано.
package-detail-not-loaded = Деталі пакунка ще не завантажено.
package-copying-local = Копіювання до локального пакунка «{ $name }»…
package-parse-manifest-failed = Не вдалося проаналізувати маніфест: { $error }
package-fork-took-over = Ви редагуєте локальний пакунок «{ $name }». Тепер він має пріоритет у профілях, де цей пакунок увімкнено. Установлений пакунок залишається резервним.
package-fork-took-over-toast = Тепер ви редагуєте локальний пакунок { $name }.
package-fork-mirrored = Ви редагуєте локальний пакунок «{ $name }». Тепер він має пріоритет, але вимкнений у всіх профілях. Установлений пакунок залишається резервним.
package-fork-mirrored-toast = Створено вимкнене локальне заміщення { $name }.
package-fork-inactive = Ви редагуєте окремий локальний пакунок «{ $name }». Спочатку він вимкнений у всіх профілях.
package-fork-inactive-toast = Створено локальну копію { $name } (вимкнена).
package-fork-failed = Не вдалося відредагувати копію: { $error }
package-required-unavailable = Обов'язковий пакунок { $name } недоступний: { $error }
package-requirer-unavailable = Smudgy не може перевірити вимоги встановленого пакунка { $name }: { $error }
package-required-manifest-invalid = Пакунок { $name } має недійсний маніфест: { $error }
package-install-required-unavailable = Обов'язковий пакунок недоступний
package-required-managed = Його потребує { $packages }.
package-install-independently = Встановити незалежно
package-requirements-refresh-incomplete = Не вдалося повністю оновити відомості про обов'язкові пакунки: { $error }. Перевірте, чи ці пакунки встановлено та чи доступні їхні маніфести.
package-folder-locate-failed = Не вдалося знайти теку: { $error }
package-folder-missing = Цієї теки пакунка ще не існує.
package-folder-open-failed = Не вдалося відкрити теку: { $error }
package-rename-failed = Не вдалося перейменувати: { $error }
package-renamed = Перейменовано на { $name }.
package-trust-update-failed = Не вдалося оновити довіру: { $error }
package-unsandboxed-toast = Вимкнено пісочницю: { $name }
package-sandboxed-toast = Увімкнено пісочницю: { $name }
package-local-unsandboxed-toast = { $name } тепер працює без пісочниці (повний доступ).
package-local-sandboxed-toast = { $name } повернувся до пісочниці зі свого маніфесту.
package-consent-record-failed = Не вдалося зберегти згоду: { $error }
package-permissions-updated = Оновлено дозволи для { $name }
package-not-found = Пакунок «{ $name }» не знайдено.
package-load-failed = Не вдалося завантажити пакунок «{ $name }»: { $error }
package-file-read-failed = Не вдалося прочитати { $path }: { $error }
package-file-save-failed = Не вдалося зберегти: { $error }
package-file-changed-outside = Файл { $path } змінено поза Smudgy. Відкрийте його знову та перевірте зміни перед збереженням.
package-file-saved = Збережено { $path }.
package-publishing-progress = Генерування декларацій і публікація { $name }…
package-delete-failed = Не вдалося видалити: { $error }
package-deleted-toast = Видалено пакунок { $name }.
package-deleted-with-warning-toast = Видалено пакунок { $name }. Частину очищення не завершено: { $error }
package-create-failed = Не вдалося створити пакунок: { $error }
package-install-failed = Не вдалося встановити: { $error }
package-install-plan-changed = Набір пакунків змінився, поки це вікно було відкрито. Перевірте встановлення ще раз.
package-installed-consent-failed = Встановлено, але не вдалося зберегти згоду: { $error }
package-required-install-failed = Не вдалося встановити обов'язковий пакунок { $name }: { $error }
package-installed-enabled-toast = Встановлено й увімкнено { $name }.
package-updated-toast = { $name } оновлено.
package-installed-review-toast = Встановлено { $name }.
package-field-required = Поле «{ $field }» обов'язкове.
package-field-invalid = «{ $field }»: { $reason }
package-field-save-failed = Не вдалося зберегти «{ $field }»: { $error }
package-settings-save-failed = Smudgy не може зберегти налаштування пакунка: { $error }
package-settings-state-changed = Налаштування змінено в іншому місці. Відкрийте їх знову перед збереженням.
package-settings-saved = Налаштування збережено.
package-clear-secret-failed = Не вдалося очистити «{ $field }»: { $error }
package-secret-cleared = Секрет очищено.
package-version-invalid-semver = «{ $version }» не є коректною семантичною версією (наприклад, 1.2.3). Змініть версію в розділі Маніфест вище.
package-version-build-metadata = Версія не може містити метаданих збірки (видаліть суфікс +…).

# Remaining app-shell and editor surfaces
badge-dependency = ЗАЛ.
badge-required = ВИМ.
badge-dependency-required = ЗАЛ. + ВИМ.
editor-example-alias-name = напр., kill
editor-example-hotkey-name = напр., north
editor-example-trigger-name = напр., low-health-alert
editor-example-folder-path = напр., combat/healing
editor-example-module-path = напр., lib/util.ts
link-confirm-open-title = Сервер хоче відкрити посилання у вашому браузері
link-confirm-send-title = Сервер хоче надіслати команду від вашого імені
link-confirm-allow-host = Завжди дозволяти посилання на { $host }
link-confirm-trust-server = Завжди довіряти посиланням із цього сервера
link-tooltip-loading = Завантаження…
map-another-map = інша мапа
map-another-server = інший сервер
map-rooms-match-elsewhere = Кімнати тут збігаються з «{ $name }» (показується на { $servers })
map-show-here-too = Показати також тут
window-none-open = Немає відкритих вікон
automation-save-aliases-failed = Не вдалося зберегти аліаси: { $error }
automation-save-hotkeys-failed = Не вдалося зберегти гарячі клавіші: { $error }
automation-save-triggers-failed = Не вдалося зберегти тригери: { $error }
automation-save-state-failed = Не вдалося зберегти стан автоматизації: { $error }
automation-state-baseline-unavailable = Smudgy не може зберегти зміни, оскільки поточний стан автоматизації недоступний. Перезавантажте дані та спробуйте ще раз.
automation-save-state-conflict = Інший процес змінив ці автоматизації. Перезавантажте їх перед збереженням змін.
automation-script-folder-invalid = Не вдалося завантажити скрипт, призначений до «{ $path }», що не є текою.
automation-script-folder-create-failed = Не вдалося створити теку скриптів.
map-this-map = Ця мапа
map-unknown = Невідома мапа
manifest-host-example = напр., aardwolf.org

# Session runtime feedback
runtime-loading-session = Завантаження сесії…
runtime-reloading-scripts = Перезавантаження скриптів…
runtime-session-rule = { $profile } на { $server }
runtime-loading-packages = Завантаження пакунків…
runtime-loaded-packages-clause = { $count ->
        [one] Завантажено { $count } пакунок (за { $milliseconds } мс)
        [few] Завантажено { $count } пакунки (за { $milliseconds } мс)
        [many] Завантажено { $count } пакунків (за { $milliseconds } мс)
       *[other] Завантажено { $count } пакунка (за { $milliseconds } мс)
    }
runtime-loaded-modules-clause = { $count ->
        [one] { $count } модуль скриптів (за { $milliseconds } мс)
        [few] { $count } модулі скриптів (за { $milliseconds } мс)
        [many] { $count } модулів скриптів (за { $milliseconds } мс)
       *[other] { $count } модуля скриптів (за { $milliseconds } мс)
    }
runtime-loaded-maps-clause = { $count ->
        [one] { $count } область мап (за { $milliseconds } мс)
        [few] { $count } області мап (за { $milliseconds } мс)
        [many] { $count } областей мап (за { $milliseconds } мс)
       *[other] { $count } області мап (за { $milliseconds } мс)
    }
runtime-loaded-maps-shared = ({ $owned } власних, { $shared } наданих)
runtime-opened-offline = Відкрито офлайн · Підключитися
runtime-connecting-to = Підключення до { $host }…
runtime-connected-to = Підключено до { $host }
runtime-connection-failed = Помилка підключення: { $error }
runtime-connection-abandoned = Відключено
runtime-send-error = Помилка надсилання: { $error }
runtime-reconnecting-to = Перепідключення до { $host }…
runtime-not-sent-disconnected = З’єднання було розірвано.
runtime-not-sent-still-connecting = Підключення ще триває.
runtime-not-sent-reconnect-failed = Не вдалося перепідключитися.
runtime-not-sent-abandoned = Відключено.
runtime-not-sent-not-connected = Немає з’єднання.
runtime-not-sent-could-not = { $text } не вдалося надіслати.
runtime-not-sent-was-not = { $text } не надіслано.
runtime-not-sent-script-dropped = Скрипт намагався надіслати { $text }, але це було відкинуто.
runtime-not-sent-reconnecting = Перепідключення…
runtime-not-sent-try-again = Спробувати ще раз
runtime-not-sent-binary = двійкові дані
runtime-not-sent-empty = порожній рядок
runtime-maps-auth-required = Мапи недоступні. Увійдіть або створіть обліковий запис Smudgy, щоб користуватися цією функцією.
runtime-maps-load-failed = Не вдалося завантажити мапи: { $error }
runtime-error = Помилка середовища виконання: { $error }
runtime-max-depth = Помилка: перевищено максимальну глибину виконання
runtime-process-line-error = Помилка обробки рядка { $error }
runtime-process-partial-line-error = Помилка обробки неповного рядка { $error }
runtime-process-command-error = Помилка обробки команди { $error }
runtime-javascript-error = Помилка JavaScript: { $error }
runtime-javascript-function-error = Помилка у функції JavaScript: { $error }
runtime-call-javascript-function-error = Помилка під час виклику функції JavaScript: { $error }
runtime-add-script-error = Помилка під час додавання скрипту: { $error }
runtime-gmcp-goodbye = GMCP: сервер прощається.
runtime-gmcp-goodbye-reason = GMCP: сервер прощається: { $reason }
runtime-package-required-params-missing = { $name } ще не налаштовано. Налаштувати зараз.
runtime-package-not-loaded-reason = [package] { $name } не завантажено — { $reason }.
runtime-package-not-loaded-error = [package] { $name } не завантажено — { $error }
runtime-packages-modules-load-failed = [packages] не вдалося завантажити модулі — { $error }
runtime-package-needs-permissions = [package] { $name } не завантажено — доступні версії потребують більше дозволів, ніж надано. Відкрийте Автоматизації, щоб переглянути й надати оновлення.
runtime-package-no-version = [package] { $name } не завантажено — не знайдено жодної його версії (могла бути видалена або знята з публікації, чи хмара недосяжна). Відкрийте Автоматизації, щоб видалити його чи перевстановити.
runtime-package-sandbox-build-failed = [package] { $name } не зміг побудувати свою пісочницю дозволів — { $error }
runtime-package-isolate-start-failed = [package] { $name } не зміг запустити свій ізолят — { $error }
runtime-package-load-failed = [package] { $name } не зміг завантажитися — { $error }
runtime-package-no-backend = [package] { $name } не завантажено: немає бекенду пакунків для цієї сесії
runtime-package-updated = [package] { $name } оновлено { $from } → { $to }
runtime-package-duplicate-versions = [package] попередження: { $name } завантажено в { $count } паралельних версіях ({ $versions }) — побічні ефекти можуть конфліктувати; розгляньте розгалуження або закріплення
package-version-floor-invalid = { $package } оголошує непридатне значення min_smudgy_version («{ $version }» не є версією semver); пакунок потребує виправленого випуску
package-version-floor-newer = { $package } потребує Smudgy { $required } або новішої — це Smudgy має версію { $running }; оновіть Smudgy, щоб його використати
widget-text-editor-unavailable = Текстовий редактор недоступний
widget-map-unavailable = Мапа недоступна
widget-map-no-mapper = Мапа недоступна (немає мапера)
validation-name-empty = Назва не може бути порожньою
validation-name-too-long = { $max ->
        [one] Назва не може бути довшою за { $max } символ
        [few] Назва не може бути довшою за { $max } символи
       *[other] Назва не може бути довшою за { $max } символів
    }
validation-name-character = Назва не може містити символ «{ $character }»
validation-name-control = Назва не може містити символів керування
validation-name-whitespace = Назва не може містити табуляцій чи символів кінця рядка
validation-name-dot-edge = Назва не може починатися чи закінчуватися символом «.»
validation-name-reserved = «{ $name }» — зарезервована назва
validation-folder-empty = Назва теки не може бути порожньою
validation-folder-empty-segment = Шлях теки не може містити порожніх сегментів
validation-module-empty = Назва модуля не може бути порожньою
validation-module-leading-slash = Назва модуля не може починатися з «/»
validation-module-empty-segment = Шлях модуля не може містити порожніх сегментів
validation-module-traversal = Шлях модуля не може містити сегментів «.» чи «..»
validation-package-empty = Назва пакунка не може бути порожньою
validation-package-too-long = { $max ->
        [one] Назва пакунка не може бути довшою за { $max } символ
        [few] Назва пакунка не може бути довшою за { $max } символи
       *[other] Назва пакунка не може бути довшою за { $max } символів
    }
validation-package-characters = Назви пакунків можуть містити лише літери, цифри, «-» та «_»

# Ordinary package settings recovery and transfer
package-settings-copy = Копіювати
package-settings-paste = Вставити…
package-settings-history = Історія…
package-settings-reset = Скинути…
package-settings-no-secrets = Секрети залишаться без змін.
package-settings-copied-clipboard = Збережені налаштування скопійовано.
package-settings-unset = Не задано
package-settings-history-empty = Попередніх налаштувань ще немає.
package-settings-no-changes = Ці налаштування вже використовуються.
package-settings-skipped = Недоступні або несумісні налаштування буде пропущено: { $keys }
package-settings-forget = Видалити історію…
package-settings-forget-confirm = Поточні налаштування залишаться доступними, зокрема після перевстановлення.
package-settings-reset-confirm = Відновити типові значення. Попередні налаштування залишаться в Історії.
package-settings-apply = Застосувати
package-settings-restored = Попередні налаштування відновлено.
package-settings-restored-partial = Налаштування відновлено. Несумісні значення залишаються в історії: { $keys }
package-settings-unsaved-warning = Незбережені зміни буде замінено.
package-settings-script-change = Скрипт

package-settings-diff-title = Зміни після відновлення
package-settings-diff-more = …і ще { $count } змін
package-settings-diff-skipped = Не можна застосувати налаштувань: { $count }
package-settings-value-empty = Порожньо
package-settings-value-on = Увімкнено
package-settings-value-off = Вимкнено
package-settings-change-add = Додати «{ $value }»
package-settings-change-remove = Видалити «{ $value }»
package-settings-change-replace = Змінити з «{ $before }» на «{ $after }»
package-settings-change-reorder = Змінити порядок наявних записів
package-settings-change-entry = { $setting } · Запис { $number }
package-settings-change-add-row = Додати рядок: { $details }
package-settings-change-remove-row = Видалити рядок: { $details }
package-settings-history-title = Історія налаштувань
package-settings-paste-title = Вставити налаштування
package-settings-restore-title = Відновити налаштування
package-settings-reset-title = Скинути налаштування?
package-settings-forget-title = Видалити історію?
package-settings-paste-action = Вставити
package-settings-restore-action = Відновити
package-settings-all-profiles = Усі профілі
package-settings-required = * Обов’язкове
package-settings-copied-with-secrets = Збережені налаштування скопійовано. Без секретів.

# Link editor: a room's Exits (compass, one row per link) and a selected link
compass-north = Пн
compass-northeast = ПнСх
compass-east = Сх
compass-southeast = ПдСх
compass-south = Пд
compass-southwest = ПдЗх
compass-west = Зх
compass-northwest = ПнЗх
compass-up = Вг
compass-down = Вн
compass-in = всер
compass-out = наз
compass-special = спец
compass-other = інше
mapper-room-name = #{ $number }
mapper-room-name-titled = #{ $number } · { $title }
mapper-place-room-name = { $place } #{ $number }
mapper-place-room-name-titled = { $place } #{ $number } · { $title }
mapper-other-map-prefix = { $map } ›
link-nowhere = (без призначення)
link-add-exit = + Додати вихід…
link-new-exit = Новий вихід ({ $direction }) з: { $room }
link-leaves = виходить
link-arrives-from = приходить з
link-weight = вага
link-command = команда
link-heading = З'єднання
link-change = Змінити ▾
link-change-close = Закрити ▴
link-two-way = ⇄ двобічне
link-one-way = → однобічне
link-swap = ⇅ поміняти кінці
link-return-elsewhere = Зворотний шлях зберігається в: { $map }.
link-two-way-would-show = Зворотний шлях бачитиме кожен, хто читає: { $map }.
link-door-name-too-long = Назва дверей задовга (ліміт символів: { $limit }).
link-opens-with-too-long = Команда, що відчиняє двері, задовга (ліміт символів: { $limit }).
link-doors = Двері
link-same-doors = однакові з обох боків
link-door-both = Двері з обох боків
link-door-one = Двері
link-door-side = Двері з боку: { $room }
link-door-none = Немає
link-door-open = Відчинені
link-door-closed = Зачинені
link-door-locked = Замкнені
link-hidden-exit = прихований вихід
link-named = з назвою
link-opens-with = відчиняє
link-door-default-name = door
link-opens-with-placeholder = open { $name }
link-hidden = прихований
link-door = { $state ->
    [open] відчинені двері
    [closed] зачинені двері
   *[locked] замкнені двері
}
link-door-named = «{ $name }», { $state ->
    [open] відчинені
    [closed] зачинені
   *[locked] замкнені
}
link-segments = Відрізки
link-corners = Кути
link-line = Лінія
link-thickness = Товщина
link-color-reset = Скинути
link-ports = Порти
link-remove = Видалити з'єднання
link-picker-help = Клацніть кімнату на мапі або знайдіть її
link-picker-placeholder = Назва кімнати або #номер
link-picker-map-placeholder = Назва мапи
link-picker-this-map-group = Ця мапа
link-picker-places-group = Секрети на цій мапі
link-picker-other-map = Інша мапа…
link-picker-other-maps = Інші мапи
link-picker-this-map = Назад до цієї мапи
link-picker-nothing = Жодна кімната не підходить.
link-picker-no-destination = Поки без призначення
link-picker-more = { $count ->
    [one] ще { $count } — введіть, щоб звузити
    [few] ще { $count } — введіть, щоб звузити
    [many] ще { $count } — введіть, щоб звузити
   *[other] ще { $count } — введіть, щоб звузити
}
mapper-link-not-changed = Не вдалося змінити з'єднання.

# Scoped group permissions.

permissions-tab-clan = Клан
permissions-title = Дозволи групи { $group }
permissions-manage = Керувати дозволами
permissions-select-resource = Виберіть теку, мапу або пакунок, щоб керувати дозволами цієї групи.
permissions-other-maps = Мапи поза переліченими теками
permissions-owners-implicit = Власники клану мають повні дозволи в усьому клані, крім мап і секретів, що належать учасникам.
permissions-folder-inheritance = Дозволи охоплюють цю теку та поточні й майбутні мапи клану в ній. Мапи учасників зберігають власні правила доступу.
permissions-direct-note = Дозволи від інших надань діють, доки їх не змінять у джерелі. Прямі дозволи можуть додати доступ, але не заборонити успадкований.
permissions-inherited = Також надано через { $scope }: { $permissions }.
permissions-edit-origin = Змінити початкове надання
permissions-delegation-title = Дозволи, які група може надавати іншим
permissions-delegation-help = Виберіть дозволи, які учасники можуть надавати на цей ресурс. Надання залишаються в цих межах і закінчуються разом із делегуванням. Керування доступом саме по собі не дозволяє читати або редагувати вміст.
permission-help-clan-edit-profile = Змінювати назву й опис клану.
permission-help-clan-read-members = Переглядати список учасників клану.
permission-help-clan-invite = Запрошувати до клану.
permission-help-clan-revoke-invitation = Скасовувати очікувані запрошення до їх прийняття.
permission-help-clan-remove-member = Вилучати звичайних учасників і припиняти їхній доступ до клану. Лише власники клану можуть змінювати власність.
permission-help-group-create = Створювати групи. Автор приєднується до нової групи й може керувати нею, доки є учасником клану.
permission-help-group-rename = Змінювати назви й кольори груп без зміни дозволів або членства.
permission-help-group-delete = Видаляти користувацькі групи й доступ, наданий ними. Вбудовані групи видалити не можна.
permission-help-group-assign = Додавати й вилучати учасників. Вони отримують або втрачають дозволи групи, включно з доступом до її Секретів. Додати себе до чужої групи може лише власник клану.
permission-help-group-inspect-assignments = Дозволити учасникам цієї групи бачити, хто входить до кожної групи.
permission-help-atlas-create = Створювати теки клану. Доступ у кожній теці треба надати окремо; це не дає доступу до наявних мап.
permission-help-package-create = Створювати пакунки клану. Редагування й публікація потребують дозволів на новий пакунок. Інші пакунки мають власні дозволи.
permission-help-atlas-read = Бачити цю теку. Читання мап усередині потребує окремого дозволу.
permission-help-atlas-rename = Перейменовувати теку. Її мапи й дозволи залишаються на місці.
permission-help-atlas-delete = Видаляти теку за правилами клану. Цей дозвіл мають лише власники клану.
permission-help-atlas-accept-filing = Дозволяти переміщення мап до цієї теки. Переміщення також потребує дозволу перемістити саму мапу з поточної теки.
permission-help-atlas-accept-transfer = Переміщувати власні мапи до цієї папки клану. Виберіть, чи стане клан їхнім власником, чи ви збережете право власності. Наявні спільні доступи до мап буде припинено.
permission-help-area-create = Створювати мапи клану в цій теці. Автор може редагувати нову мапу; власники клану зберігають повноваження власника.
permission-help-area-create-member-owned = Створювати тут мапи учасників. Доступ контролюють їхні записані власники; дозволи теки й власність клану їх не розкривають.
permission-help-area-read = Читати звичайний вміст мапи. Це не розкриває Секрети або приватні доповнення інших людей. Читання Секрету також потребує доступу до мапи. На мапах учасників читання залишається ввімкненим, доки надано будь-який інший дозвіл на мапу.
permission-help-area-add = Додавати звичайний вміст мапи. Читання мапи все одно потрібне. Це не дозволяє редагувати наявний вміст або писати в Секретах.
permission-help-area-edit = Змінювати наявний звичайний вміст мапи. Потрібне читання мапи. Редагування Секретів має окремі дозволи.
permission-help-area-remove-content = Видаляти звичайний вміст мапи. Видалення кімнати також видаляє прикріплені секретні й приватні доповнення. Це не дозволяє видаляти всю мапу.
permission-help-area-rename = Перейменовувати мапу без зміни її власності або вмісту.
permission-help-area-refile = Переміщувати мапу до іншої теки клану, що приймає мапи. Дозволи цільової теки можуть змінити доступ до неї.
permission-help-area-delete = Видаляти всю мапу разом із Секретами й приватними доповненнями. Це окремо від видалення звичайного вмісту.
permission-help-area-copy = Створювати незалежну копію. Секрет копіюється лише з власним дозволом копіювання; приватні доповнення інших людей ніколи не копіюються.
permission-help-area-share-external = Надавати друзям поза кланом перегляд мапи без редагування, копіювання або Секретів. Доступ закінчується, коли автор надання виходить із клану або втрачає цей дозвіл.
permission-help-secret-create-member-owned = Створювати Секрети учасників на доступній для читання мапі. Їхні власники вибирають одержувачів; власність клану сама по собі не дає доступу.
permission-help-secret-create-clan-owned = Створювати Секрети клану на доступних мапах клану. Автор може читати, доповнювати й редагувати новий Секрет. Власники клану зберігають усі повноваження.
permission-help-secret-read = Читати Секрети клану на цих мапах. Читання самої мапи також потрібне. Це ніколи не розкриває Секрети учасників.
permission-help-secret-add = Додавати вміст до Секретів клану. Мапа й Секрет мають бути доступними для читання. Звичайний вміст мапи має окремі дозволи.
permission-help-secret-edit = Змінювати вміст Секретів клану. Мапа й Секрет мають бути доступними для читання. Читач мапи може редагувати Секрет без права редагування самої мапи.
permission-help-secret-remove-content = Видаляти вміст Секретів клану. Мапа й Секрет мають бути доступними для читання. Це не дозволяє видаляти весь Секрет або змінювати Секрети учасників.
permission-help-secret-manage-access = Переглядати й відкликати надання Секретів клану та надавати наявні у вас дозволи. Це не дає власності або контролю над Секретами учасників.
permission-help-secret-copy = Додавати Секрети клану до копії мапи. Мапа також потребує дозволу копіювання. Копії незалежні від подальших змін доступу.
permission-help-grant-inspect = Переглядати надання для цього ресурсу та ресурсів у його межах. Це саме по собі не розкриває Секрети й не дозволяє змінювати доступ.
permission-help-grant-manage = Надавати доступ у цих межах і за обмеженнями нижче. Лише власники клану можуть надати цей дозвіл. Він не дає власності.
permission-help-package-read = Показувати пакунок у ресурсах клану. Учасники все одно можуть установлювати опубліковані пакунки клану за назвою без цього дозволу. Він не дає права публікації або редагування.
permission-help-package-edit-draft = Завантажувати й змінювати чернетку пакунка. Публікація потребує окремого дозволу.
permission-help-package-edit-metadata = Змінювати метадані пакунка. Публікація та зміна доступності потребують окремих дозволів.
permission-help-package-manage-availability = Змінювати публічну доступність пакунка. Публічний пакунок можуть знайти й завантажити люди поза кланом.
permission-help-package-publish = Публікувати нові версії. Люди, які встановили пакунок, можуть отримати новий код під час оновлення.
permission-help-package-retire = Відкликати версії пакунка за правилами реєстру. Це не видаляє весь пакунок.
permission-help-package-delete = Видаляти пакунок за правилами реєстру. Завантажені копії можуть залишитися на пристроях інших людей.
permission-help-read = Читати Секрет, коли його мапа також доступна для читання. Надання Секрету ніколи не дає доступу до самої мапи.
permission-help-add = Додавати вміст до Секрету. Редагування й видалення наявного вмісту потребують окремих дозволів.
permission-help-edit = Змінювати наявний вміст Секрету. Звичайний вміст мапи має окремі дозволи.
permission-help-remove = Видаляти вміст Секрету. Це не дозволяє видаляти весь Секрет.
permission-help-manage-access = Переглядати й відкликати надання Секрету та надавати наявні у вас дозволи. Лише власники можуть призначати керівників доступу або змінювати власність.
permission-help-copy = Додавати Секрет до незалежної копії мапи. Мапа також потребує дозволу копіювання. Подальше відкликання не видаляє вже зроблені копії.

permissions-clan-note = Ці дозволи діють для всього клану. Виберіть Мапи або Пакунки, щоб налаштувати доступ до окремих ресурсів.
permissions-section-clan = Клан і членство
permissions-section-groups = Керування групами
permissions-section-creation = Створення ресурсів
permissions-section-access = Керування доступом
permissions-section-secret-creation = Створення Секретів

move-review-loading = Перевірка змін доступу…
move-review-selection = Кімнати: { $rooms } · Зв’язки: { $links } · Мітки: { $labels } · Фігури: { $shapes }
move-review-destination-notice = Переміщений вміст підпорядковуватиметься дозволам місця призначення, зокрема доступу, який ви не можете перевірити.
move-review-filing-notice = Переміщення цієї мапи може змінити доступ інших учасників до Секретів, які ви не можете перевірити. Власники Секретів і явні налаштування спільного доступу залишаться незмінними.
move-review-group = { $name }, включно з майбутніми учасниками
move-review-members = Учасники клану, включно з майбутніми
move-review-and = { " ТА " }
move-review-overlap = Ці аудиторії можуть отримати або втратити перелічені дозволи. Інші надання дозволів можуть зберегти доступ для деяких учасників.
move-review-gain = Можуть отримати: { $actions }
move-review-loss = Можуть втратити: { $actions }
move-review-share-right = Надавати дозвіл: { $action }
move-review-right-read = Читання
move-review-right-add = Додавання
move-review-right-edit = Редагування
move-review-right-remove = Видалення вмісту
move-review-right-copy = Копіювання
move-review-right-manage-access = Керування доступом
move-review-right-rename = Перейменування
move-review-right-delete = Видалення
move-review-right-manage-ownership = Керування власністю

mapper-clipboard-one-source = Вибирайте вміст одного джерела за раз.
mapper-cut-remove-required = Переміщення потребує дозволу Видалення у вихідному джерелі та Додавання в цільовому.
mapper-cut-ready = Готово до переміщення. Оригінал залишиться до успішного вставлення.
mapper-cut-cross-map-unavailable = Переміщення між мапами поки не підтримується. Оригінал не змінено. Скористайтеся копіюванням, щоб створити дублікат.
mapper-cut-stale = Джерело змінилося після вирізання. Виділіть вміст і виріжте його знову.
mapper-cut-same-source = Виберіть інше джерело перед вставленням або скористайтеся Вставити тут, щоб змінити розташування.
move-review-preserve-positions = Переміщення між джерелами зберігає розташування об’єктів та наявні посилання на кімнати.

mapper-copy-source-denied = Для цього вмісту потрібен дозвіл на копіювання його джерела.

cloud-error-access-review-required = Перш ніж підтвердити переміщення, перегляньте, хто отримає або втратить доступ. Нічого не переміщено.
cloud-error-stale-access-review = Після перегляду доступ змінився. Почніть переміщення знову, щоб переглянути поточний результат. Нічого не переміщено.
cloud-error-move-property-conflict = Місце призначення містить інші значення деяких властивостей кімнат. Узгодьте ці значення перед переміщенням. Початковий вміст не змінено.

mapper-additional-room-data = Додаткові дані кімнати

move-property-to = Перемістити до…
move-review-properties = Вибрані властивості: { $count }
move-review-property-no-undo = Це переміщення об’єднує наявні дані, і його не можна скасувати. Виберіть значення для кожної конфліктної властивості.
move-review-property-source = Вхідне: { $value }
move-review-property-destination = У призначенні: { $value }
move-review-property-keep-source = Залишити вхідне
move-review-property-keep-destination = Залишити цільове

mapper-attached-rooms = Кімнати з даними в цьому джерелі

area-list-filter-placeholder = Фільтрувати мапи й папки…

area-list-no-matches = Відповідних мап або папок немає.

mapper-multi-transferred = «{ $name }» переміщено до клану.

clan-maps-no-transfer-folder = Немає папок клану, до яких ви можете перемістити цю мапу.
