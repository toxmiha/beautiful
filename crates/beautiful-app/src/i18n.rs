//! Built-in RU/EN strings + addon language packs.
//!
//! Source UI strings (Russian or English) are looked up against the active
//! language. Add-ons may register extra language codes via `register_language`
//! / `set_translation` (permission `i18n`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const PAIRS: &[(&str, &str)] = &[
    // (ru, en) — lookup: EN uses ru→en, RU uses en→ru. Do not duplicate reversed.
    // Gallery / home
    ("Моя галерея", "My gallery"),
    ("Общее время в программе", "Total time in the app"),
    ("Время в холсте", "Time on canvas"),
    ("Подвкладки", "Sheets"),
    ("+ Подвкладка", "+ Sheet"),
    ("Создать (пустой)", "Create (empty)"),
    ("Из буфера обмена", "From clipboard"),
    ("Открыть…", "Open…"),
    ("Открыть холст…", "Open canvas…"),
    ("Посмотреть запись", "Watch recording"),
    ("Для этого холста пока нет записи", "This canvas has no recording"),
    ("Журнал действий", "Action journal"),
    ("не видео", "not video"),
    ("Воспроизведение", "Play"),
    ("Пауза", "Pause"),
    ("Скорость", "Speed"),
    ("Сохранить видео…", "Save video…"),
    ("Сохранить видео", "Save video"),
    ("Экспорт видео…", "Exporting video…"),
    ("Видео сохранено", "Video saved"),
    ("MP4 / WebM / GIF · текущая скорость", "MP4 / WebM / GIF · current speed"),
    ("Нужен ffmpeg (dist/ffmpeg или PATH)", "Need ffmpeg (dist/ffmpeg or PATH)"),
    ("Сжатие", "Compression"),
    ("Качество", "Quality"),
    ("Баланс", "Balanced"),
    ("Компакт", "Compact"),
    ("Минимум", "Tiny"),
    ("Ватермарка", "Watermark"),
    ("Выбрать изображение…", "Choose image…"),
    ("Выбрать ватермарку", "Choose watermark"),
    ("Выбрать музыку", "Choose music"),
    ("Выбрать файл…", "Choose file…"),
    ("Убрать", "Remove"),
    ("Позиция", "Position"),
    ("Наклон", "Rotation"),
    ("Прозрачность", "Opacity"),
    ("Наложение", "Blend"),
    ("Музыка", "Music"),
    ("Перетащи на кадр", "Drag on the frame"),
    ("громкость — колонка справа", "volume — speaker on the right"),
    ("Вписать все", "Fit all"),
    ("Стол", "Desk"),
    ("Формат", "Format"),
    ("Коллекция", "Collection"),
    ("Без коллекции", "No collection"),
    ("Теги", "Tags"),
    ("Новый холст", "New canvas"),
    ("Новая коллекция", "New collection"),
    ("Недавние", "Recent"),
    ("Все холсты", "All canvases"),
    ("Критерии поиска", "Search filters"),
    ("Отмена", "Cancel"),
    ("Сохранить", "Save"),
    ("Создать", "Create"),
    ("Имя", "Name"),
    ("Размер", "Size"),
    ("Ориентация", "Orientation"),
    ("Альбомная", "Landscape"),
    ("Книжная", "Portrait"),
    ("Разрешение", "Resolution"),
    ("Цвет фона", "Background color"),
    ("Библиотека", "Library"),
    ("Очистить", "Clear"),
    ("+ Добавить", "+ Add"),
    ("Из библиотеки", "From library"),
    ("Предпросмотр", "Preview"),
    ("Название файла", "File name"),
    ("Белый", "White"),
    ("Чёрный", "Black"),
    ("Фон UI", "UI background"),
    ("Серый", "Gray"),
    ("Прозрачный", "Transparent"),
    ("Свой", "Custom"),
    ("(blur на главной)", "(blur on home)"),
    // Language / prefs
    ("Язык", "Language"),
    ("Русский", "Russian"),
    ("Английский", "English"),
    ("Настройки", "Preferences"),
    ("Система", "System"),
    ("Интерфейс", "Interface"),
    ("Ввод", "Input"),
    ("Форматы файлов", "File Formats"),
    ("Горячие клавиши", "Keymap"),
    ("Аддоны", "Add-ons"),
    ("Категории", "Categories"),
    ("Сбросить оформление", "Reset Appearance"),
    ("Сбросить все настройки", "Reset All Settings"),
    ("Обзор…", "Browse…"),
    ("Открыть", "Open"),
    ("Материал окна", "Window material"),
    ("Тема", "Theme"),
    ("Тёмная", "Dark"),
    ("Светлая", "Light"),
    ("Показывать статус в Discord", "Show status in Discord"),
    ("Заголовок", "Title"),
    (
        "Локальный IPC с Discord desktop (как у игр). Картинка холста никуда не загружается. В Developer Portal → Art Assets ключ logo.",
        "Local IPC with Discord desktop (like games). The canvas image is never uploaded. Developer Portal → Art Assets key: logo.",
    ),
    (
        "Всегда: время сессии · инструмент · размер холста · слои. NSFW → имя скрыто. Превью холста нет.",
        "Always: session time · tool · canvas size · layers. NSFW hides the name. No canvas preview.",
    ),
    ("Обновить статус сейчас", "Update status now"),
    ("Название приложения", "App name"),
    ("Название холста", "Canvas name"),
    ("Поиск", "Search"),
    (
        "Русский и английский встроены. Аддоны могут добавлять другие языки.",
        "Russian and English are built in. Add-ons can register extra languages.",
    ),
    ("Непрозрачные панели, без размытия", "Opaque panels, no blur"),
    ("Полупрозрачное размытие", "Translucent blur tint"),
    (
        "Подложка с оттенком обоев",
        "Wallpaper-tinted opaque backdrop",
    ),
    ("Матовое стекло + яркий край", "Frosted glass + bright edge"),
    (
        "Классическое стекло + блик",
        "Legacy glass blur + gloss",
    ),
    ("Дымка поверх панелей", "Dim smoke overlay chrome"),
    ("Сила подложки — сразу", "Backdrop strength — live"),
    (
        "Показывать FPS / память / диск в строке статуса",
        "Show FPS / Mem / Drive in status bar",
    ),
    (
        "Показывать строку статуса и панель инструмента (FPS / память / диск)",
        "Show status bar and tool options (FPS / Mem / Drive)",
    ),
    (
        "Также видно, когда открыт профилировщик F12",
        "Also shown while the F12 profiler is open",
    ),
    (
        "Отладка: FPS, кадр, LOD, Mem%, диск. Масштаб холста всегда виден.",
        "Debug HUD: FPS, frame time, LOD, Mem%, Drive%. Zoom stays visible.",
    ),
    ("Масштаб холста", "Canvas zoom"),
    ("Шаг на щелчок колеса (%)", "Step per mouse-wheel click (%)"),
    (
        "Плавный масштаб (тачпад / непрерывно)",
        "Smooth zoom (trackpad / continuous)",
    ),
    (
        "Выкл = дискретные шаги (стабильно). Вкл = непрерывно; точка под курсором не прыгает.",
        "Off = discrete steps (stable, stepped). On = continuous; pivot stays locked so the canvas does not shake.",
    ),
    ("Стрелки — панорама", "Arrow keys pan"),
    (
        "Скорость панорамы холста (пикселей в секунду)",
        "Canvas pan speed (pixels per second)",
    ),
    (
        "С зажатым Shift (пикселей в секунду)",
        "With Shift held (pixels per second)",
    ),
    (
        "Прозрачность панелей (отдельно от DWM) — сразу",
        "Panel opacity (separate from DWM strength) — live",
    ),
    ("Масштаб интерфейса", "Interface scale"),
    ("Следовать масштабу Windows", "Follow Windows display scale"),
    (
        "Доп. масштаб поверх DPI Windows (1.0 = без надбавки)",
        "Extra zoom on top of Windows DPI (1.0 = no extra)",
    ),
    (
        "Абсолютный масштаб UI (1.0 ≈ 100% / 96 DPI; игнорирует DPI Windows)",
        "Absolute UI scale (1.0 ≈ 100% / 96 DPI; ignores Windows DPI)",
    ),
    ("BMP (импорт)", "BMP (import)"),
    ("WebP (импорт)", "WebP (import)"),
    ("Сбросить горячие клавиши", "Reset Keymap"),
    ("Назначить новую клавишу для", "Press new shortcut for"),
    ("Esc — отмена", "Esc to cancel"),
    ("Корневая папка сейвов", "Save root folder"),
    ("Загрузка", "Loading"),
    ("Сохранение", "Saving"),
    ("Применение", "Applying"),
    ("Подождите", "Please wait"),
    ("Экспорт PNG", "Export PNG"),
    ("Экспорт JPEG", "Export JPEG"),
    ("Защита от обучения ИИ", "AI training protection"),
    ("Не юридическая гарантия", "Not a legal guarantee"),
    ("Средние частоты (JPEG)", "Mid-frequency (JPEG)"),
    ("Высокочастотный шум", "High-frequency noise"),
    ("Скрытая сетка", "Hidden grid"),
    ("Сдвиг цветности", "Chroma shift"),
    ("JPEG без прозрачности — белый фон", "JPEG has no alpha — white fill"),
    ("Фон экспорта", "Export background"),
    ("Корректирующий слой", "Correction layer"),
    ("Несохранённые изменения", "Unsaved changes"),
    // Menus
    ("Файл", "File"),
    ("Правка", "Edit"),
    ("Холст", "Canvas"),
    ("Выделение", "Selection"),
    ("Фильтры", "Filters"),
    ("Вид", "View"),
    ("Окно", "Window"),
    ("Новый холст…", "New Canvas…"),
    ("Открыть недавние", "Open Recent"),
    ("Нет недавних файлов", "No recent files"),
    ("Сохранить как…", "Save As…"),
    ("Экспорт", "Export"),
    ("Рабочее пространство", "Workspace"),
    ("Добавить подвкладку…", "Add sheet…"),
    ("Панели", "Panels"),
    ("Панели аддонов", "Add-on panels"),
    ("Сбросить раскладку", "Reset layout"),
    ("Настройки…", "Preferences…"),
    ("Снять выделение", "Deselect"),
    ("Применить трансформацию", "Commit transform"),
    ("Отразить вид по горизонтали", "Flip view horizontal"),
    ("Скрыть / показать интерфейс", "Hide / show interface"),
    ("Скрыть интерфейс", "Hide interface"),
    ("Показать интерфейс", "Show interface"),
    ("Отразить слой по горизонтали", "Flip layer horizontal"),
    ("Отразить слой по вертикали", "Flip layer vertical"),
    ("Отразить выделение по Г", "Flip selection H"),
    ("Отразить выделение по В", "Flip selection V"),
    ("Отменить", "Undo"),
    ("Повторить", "Redo"),
    ("Размер холста…", "Canvas Size…"),
    ("Копировать холст", "Copy canvas"),
    ("Вставить изображение", "Paste image"),
    ("Зеркало слоя (Г)", "Mirror layer (H)"),
    ("Зеркало слоя (В)", "Mirror layer (V)"),
    ("Растушевать выделение", "Feather selection"),
    ("Цвет холста (фон)", "Canvas color (background)"),
    ("(скоро)", "(soon)"),
    ("Ширина", "Width"),
    ("Высота", "Height"),
    ("Содержимое центрируется (crop / expand).", "Content is centered (crop / expand)."),
    ("Переименовать слой", "Rename layer"),
    ("Размер холста", "Canvas Size"),
    ("Студия фильтров", "Filter Studio"),
    ("Применить фильтры?", "Apply filters?"),
    ("Не применять", "Don’t apply"),
    ("Применить", "Apply"),
    // Panels
    ("Цвет", "Color"),
    ("Инструменты", "Tools"),
    ("Кисть", "Brush"),
    ("Навигатор", "Navigator"),
    ("Слои", "Layers"),
    ("Скрыть панель", "Hide panel"),
    ("Открепить окно", "Float window"),
    ("Прикрепить слева", "Dock left"),
    ("Прикрепить справа", "Dock right"),
    ("Прикрепить сверху", "Dock top"),
    ("Прикрепить снизу", "Dock bottom"),
    ("Перенести колонку влево", "Move column left"),
    ("Перенести колонку вправо", "Move column right"),
    ("Перенести ряд наверх", "Move row to top"),
    ("Перенести ряд вниз", "Move row to bottom"),
    // Tools / tips
    ("Карандаш", "Pencil"),
    ("Пиксельная кисть", "Pixel Brush"),
    ("Аэрограф", "Airbrush"),
    ("Миксер", "Mixer"),
    ("Ластик", "Eraser"),
    ("Кисть выделения", "Selection brush"),
    ("Ластик выделения", "Selection eraser"),
    ("Палец", "Smudge"),
    ("Размытие кистью", "Blur brush"),
    ("Заливка", "Fill"),
    ("Градиент", "Gradient"),
    ("Фигура", "Shape"),
    ("Штамп", "Clone brush"),
    ("Волшебная палочка", "Magic Wand"),
    ("Лассо", "Lasso"),
    ("Прямоугольное выделение", "Rect Select"),
    ("Эллиптическое выделение", "Ellipse Select"),
    ("Перемещение", "Move"),
    ("Трансформация", "Transform"),
    ("Деформация", "Warp"),
    ("Кадрирование", "Crop"),
    ("Рука", "Hand"),
    ("Лупа", "Zoom"),
    ("Пипетка", "Eyedropper"),
    ("Кисть (B)", "Brush (B)"),
    ("Карандаш (P)", "Pencil (P)"),
    ("Аэрограф (A)", "Airbrush (A)"),
    ("Миксер (U)", "Mixer (U)"),
    ("Ластик (E)", "Eraser (E)"),
    ("Палец (S)", "Smudge (S)"),
    ("Заливка (G)", "Fill (G)"),
    ("Градиент (Shift+G)", "Gradient (Shift+G)"),
    ("Фигура (F)", "Shape (F)"),
    (
        "Штамп (Shift+C; Alt-клик — источник)",
        "Clone brush (Shift+C; Alt-click source)",
    ),
    ("Волшебная палочка (W)", "Magic Wand (W)"),
    ("Лассо (L)", "Lasso (L)"),
    (
        "Прямоугольник (R) — удерживайте для эллипса",
        "Rect select (R) — hold for ellipse",
    ),
    (
        "Эллипс — удерживайте для прямоугольника",
        "Ellipse select — hold for rect",
    ),
    ("Трансформация (T / V)", "Transform (T / V)"),
    ("Сетка деформации", "Mesh Warp"),
    ("Кадр / рамка (C)", "Crop / Frame (C)"),
    ("Рука (H)", "Hand (H)"),
    ("Лупа (Z)", "Zoom (Z)"),
    ("Пипетка (I)", "Eyedropper (I)"),
    (
        "ПКМ-перетаскивание — переставить",
        "RMB-drag to rearrange",
    ),
    ("Перетащите холст, чтобы панорамировать.", "Drag the canvas to pan."),
    (
        "Клик — приблизить. Alt-клик — отдалить.",
        "Click to zoom in. Alt-click to zoom out.",
    ),
    (
        "Кликните по холсту, чтобы взять цвет.",
        "Click the canvas to sample a color.",
    ),
    // Options bar
    ("Кисть / палочка", "Fill/Wand"),
    ("Клон", "Clone brush"),
    ("Кадр / рамка", "Crop / Frame"),
    ("Инструмент", "Tool"),
    ("Плотность", "Density"),
    ("Жёсткость", "Hard"),
    ("Жёсткость кисти", "Hardness"),
    ("Выровненный", "Aligned"),
    ("Превью источника", "Source preview"),
    ("Прозрачность превью", "Preview opacity"),
    ("Alt+клик — источник", "Alt+click — source"),
    (
        "Выровненный: смещение источника сохраняется между штрихами. Выкл: каждый штрих начинается от Alt-источника.",
        "Aligned: keep source offset across strokes. Off: each stroke restarts from Alt source.",
    ),
    (
        "Показывать снятые пиксели внутри контура кисти до штампа.",
        "Show sampled pixels inside the brush outline before you stamp.",
    ),
    ("Допуск", "Tolerance"),
    ("Проведи линию A→B на холсте", "Draw a line A→B on the canvas"),
    ("Отзеркалить", "Mirror"),
    ("Пропорции", "Aspect"),
    ("Свободно", "Free"),
    ("Выпрямить", "Straighten"),
    ("Применить кадр", "Apply crop"),
    (
        "Enter = применить · Esc = отмена · тяните за край, чтобы расширить · кадр необратим",
        "Enter = apply · Esc = cancel · drag past edges to expand · crop is destructive",
    ),
    ("Растушёвка", "Feather"),
    ("Применить растушёвку", "Apply feather"),
    ("Ресэмпл", "Resample"),
    ("При перетаскивании", "Dragging"),
    ("Превью", "Preview"),
    ("Финал", "Final"),
    ("Режим", "Mode"),
    ("Заменить", "Replace"),
    (
        "Файл уже существует. Заменить?",
        "A file with this name already exists. Replace it?",
    ),
    ("Добавить", "Add"),
    ("Вычесть", "Subtract"),
    ("Инверсия", "Invert"),
    (
        "Новое выделение заменяет текущее",
        "New selection replaces the current one",
    ),
    (
        "Объединить с текущим выделением (Shift)",
        "Union with the current selection (Shift)",
    ),
    (
        "Вычесть из текущего выделения (Alt)",
        "Remove from the current selection (Alt)",
    ),
    (
        "Симметричная разность с текущим выделением",
        "Symmetric difference with the current selection",
    ),
    // Selection panel
    ("Свободная трансформация", "Free transform"),
    ("Деформация углов", "Corner distort"),
    ("Деформация по сетке", "Mesh warp"),
    ("Масштаб / поворот / отражение", "Scale / rotate / flip handles"),
    ("Искажение углов (2×2)", "Corner distort (2×2)"),
    ("Сетка деформации (3×3 ячейки)", "Mesh warp (3×3 cells)"),
    (
        "Сначала выделите область на холсте.",
        "Select an area on the canvas first.",
    ),
    (
        "Без выделения — берётся объект слоя (без пустых пикселей).",
        "With no selection, the layer object is used (empty pixels skipped).",
    ),
    ("Отразить / Повернуть", "Flip / Rotate"),
    ("Отразить Г", "Flip H"),
    ("Отразить В", "Flip V"),
    ("Отразить по горизонтали", "Flip horizontally"),
    ("Отразить по вертикали", "Flip vertically"),
    ("Поворот на 90° против часовой", "Rotate 90° counter-clockwise"),
    ("Поворот на 90° по часовой", "Rotate 90° clockwise"),
    // Fill / shape
    ("Смежные", "Contiguous"),
    ("Образец", "Sample"),
    ("Непрозрачность", "Opacity"),
    ("Режим наложения", "Blend mode"),
    ("Расширить", "Expand"),
    ("Сглаживание", "Anti-alias"),
    ("Сохранить / блокировать альфу", "Preserve / Lock alpha"),
    ("Игнорировать прозрачное", "Ignore transparent"),
    ("Обводка", "Stroke"),
    ("Текущий слой", "Current layer"),
    ("Текущий и ниже", "Current + below"),
    ("Все слои", "All layers"),
    ("Прямоугольник", "Rectangle"),
    ("Эллипс", "Ellipse"),
    ("Линия", "Line"),
    ("Стрелка", "Arrow"),
    ("Треугольник", "Triangle"),
    ("Звезда (5)", "Star (5)"),
    ("Звезда (4)", "Star (4)"),
    (
        "Тяните на холсте. Shift — квадрат/круг и углы линии.",
        "Drag on canvas. Shift constrains squares/circles and line angles.",
    ),
    (
        "Скорость: быстрый штрих → тоньше / светлее (к минимуму).",
        "Speed: fast stroke → thinner / lighter (toward min).",
    ),
    (
        "Shift+клик — прямая от последней точки · Shift+drag — 45°/90°",
        "Shift+click — straight from last point · Shift+drag — 45°/90°",
    ),
    (
        "Волос / резкость зарезервированы для растрового кончика (позже).",
        "Hair / sharpen reserved for bitmap tip (later).",
    ),
    ("Интервал", "Spacing"),
    ("Скорость →", "Speed →"),
    ("Размер", "Size"),
    // Keymap actions
    ("Отменить", "Undo"),
    ("Повторить", "Redo"),
    ("Повтор (альтернатива)", "Redo (alternate)"),
    ("Новый слой", "New layer"),
    ("Удалить выделение", "Delete selection"),
    ("Заливка выделения", "Fill selection"),
    ("Кисть выделения", "Selection brush"),
    ("Ластик выделения", "Selection eraser"),
    ("Прямоугольное выделение", "Rectangular select"),
    ("Размер кисти −", "Brush size −"),
    ("Размер кисти +", "Brush size +"),
    ("Приблизить", "Zoom in"),
    ("Отдалить", "Zoom out"),
    ("Масштаб 100% / сброс", "Zoom 100% / fit reset"),
    ("Инструмент лупы", "Zoom tool"),
    ("Применить тему снова", "Reapply theme"),
    ("Поменять ФГ / БГ", "Swap FG / BG"),
    ("Сброс цветов Ч/Б", "Reset colors B/W"),
    ("Новый документ", "New document"),
    ("Копировать", "Copy"),
    ("Вставить", "Paste"),
    ("Временная рука (удерж.)", "Temporary hand (hold)"),
    ("Профилировщик", "Profiler"),
    ("Панорама влево", "Pan left"),
    ("Панорама вправо", "Pan right"),
    ("Панорама вверх", "Pan up"),
    ("Панорама вниз", "Pan down"),
    // Resample
    ("Ближайший", "Nearest"),
    ("Билинейный", "Bilinear"),
    ("Бикубический", "Bicubic"),
    ("Бикубический мягче", "Bicubic Smoother"),
    ("Бикубический резче", "Bicubic Sharper"),
    ("Бикубический авто", "Bicubic Automatic"),
    ("Ланцош 3", "Lanczos3"),
    // Blend
    ("Обычный", "Normal"),
    ("Умножение", "Multiply"),
    ("Экран", "Screen"),
    ("Перекрытие", "Overlay"),
    ("Замена тёмным", "Darken"),
    ("Замена светлым", "Lighten"),
    ("Осветление основы", "Color Dodge"),
    ("Затемнение основы", "Color Burn"),
    ("Мягкий свет", "Soft Light"),
    ("Жёсткий свет", "Hard Light"),
    ("Разница", "Difference"),
    ("Исключение", "Exclusion"),
    ("Линейное осветление", "Linear Dodge"),
    ("Линейное затемнение", "Linear Burn"),
    ("Яркий свет", "Vivid Light"),
    ("Линейный свет", "Linear Light"),
    ("Точечный свет", "Pin Light"),
    ("Жёсткое смешение", "Hard Mix"),
    ("Вычитание", "Subtract"),
    ("Разделение", "Divide"),
    ("Цветовой тон", "Hue"),
    ("Насыщенность", "Saturation"),
    ("Цвет", "Color"),
    ("Светимость", "Luminosity"),
    // Filters
    ("Фильтр", "Filter"),
    ("Размытие", "Blur"),
    ("Коррекция", "Correction"),
    ("Пикселизация", "Pixelate"),
    ("Искажение", "Distort"),
    ("Эффекты", "Effects"),
    ("Размытие по Гауссу", "Gaussian Blur"),
    ("Размытие в движении", "Motion Blur"),
    ("Радиальное размытие", "Radial Blur"),
    ("Контурная резкость", "Unsharp Mask"),
    ("Яркость/Контраст", "Brightness/Contrast"),
    ("Уровни", "Levels"),
    ("Кривые", "Curves"),
    ("Канал", "Channel"),
    ("Сбросить канал", "Reset channel"),
    ("Сбросить все", "Reset all"),
    ("Цветовой тон/Насыщенность", "Hue/Saturation"),
    ("Цветовой баланс", "Color Balance"),
    ("Пикселизация", "Pixelization"),
    ("Гекс. пикселизация", "Hex Pixelization"),
    ("Треуг. пикселизация", "Triangle Pixelization"),
    ("Гекс. точки", "Hex Dots"),
    ("Постеризация", "Posterize"),
    ("Кристаллизация", "Crystallize"),
    ("Пуантилизм", "Pointillize"),
    ("Цветной растр", "Color Halftone"),
    ("Рыбий глаз", "Fisheye"),
    ("Сферическая линза", "Spherical Lens"),
    ("Рябь", "Ripple"),
    ("Скручивание", "Twist"),
    ("Хроматическая аберрация", "Chromatic Aberration"),
    ("Шум", "Noise"),
    ("Глитч", "Glitch"),
    ("Виньетка", "Vignette"),
    ("Свечение", "Glow"),
    ("Сепия", "Sepia"),
    ("Зерно плёнки", "Film Grain"),
    ("Дизеринг", "Dithering"),
    ("Замена цвета", "Replace Color"),
    ("Обводка", "Outline"),
    ("Масляная краска", "Oil Paint"),
    ("Акварель", "Watercolor"),
    ("Карандаш", "Pencil"),
    ("Пастель", "Pastel"),
    ("Текстура бумаги", "Paper Texture"),
    ("Неоновое свечение", "Neon Glow"),
    ("Лучи света", "Light Rays"),
    ("Блик объектива", "Lens Flare"),
    ("Тень", "Drop Shadow"),
    ("Фаска / рельеф", "Bevel / Emboss"),
    ("Развёртка", "Scanlines"),
    ("Жидкое стекло", "Liquid Glass"),
    ("Капля", "Droplet"),
    ("По выделению", "Selection"),
    ("Рифлёное", "Ribbed"),
    ("Градиент", "Gradient"),
    ("Оверлей", "Overlay"),
    ("Выбрать изображение…", "Choose image…"),
    ("Очистить", "Clear"),
    ("Смещение X %", "Offset X %"),
    ("Смещение Y %", "Offset Y %"),
    ("Плитка", "Tile"),
    ("Разворот", "Reverse"),
    ("Цвет A", "Color A"),
    ("Цвет B", "Color B"),
    ("Непрозрачность A", "Opacity A"),
    ("Непрозрачность B", "Opacity B"),
    ("Разброс %", "Spread %"),
    ("Толщина", "Thickness"),
    ("Округлость", "Roundness"),
    ("Следовать за штрихом", "Follow stroke"),
    ("Угол°", "Angle°"),
    ("Шаг", "Spacing"),
    ("Художественные", "Artistic"),
    ("Точно", "Fine"),
    ("Широко", "Wide"),
    ("Диапазон ползунка", "Slider range"),
    ("Пресеты…", "Presets…"),
    ("Встроенные", "Built-in"),
    ("Мои пресеты", "My presets"),
    ("Сохранить как", "Save as"),
    ("Сохранить", "Save"),
    ("Бумага", "Paper"),
    ("Цвет бумаги", "Paper color"),
    ("Углы растра", "Screen angles"),
    (
        "Клик по чипу — добавить (дубликаты OK) · × в стеке убирает",
        "Click chip to add (duplicates OK) · stack × removes one",
    ),
    (
        "Replace = классическая печать на бумаге. Overlay/Multiply = точки на рисунке (без белой заливки).",
        "Replace = classic print on paper. Overlay/Multiply = dots on your art (no forced white).",
    ),
    (
        "Overlay/Multiply оставляют исходник — без заливки бумагой.",
        "Overlay/Multiply keep the original image — no paper wash.",
    ),
    ("Пресет", "Preset"),
    ("Тонировать", "Colorize"),
    ("Метод", "Method"),
    ("Влиять", "Affect"),
    ("Активный стек", "Active stack"),
    ("Нет эффектов — нажмите чип, чтобы включить", "No effects — click a chip to enable"),
    ("Параметры", "Settings"),
    (
        "Включите фильтр, затем выберите его в стеке",
        "Enable a filter, then select it in the stack",
    ),
    (
        "Клик по чипу — вкл/выкл · × в стеке тоже выключает",
        "Click chip to enable/disable · stack × also disables",
    ),
    (
        "Инвертирует RGB активного слоя / выделения.",
        "Inverts RGB of the active layer / selection.",
    ),
    (
        "Встроенные образы и ваши сохранённые стеки",
        "Built-in looks and your saved stacks",
    ),
    (
        "Пока нет — сохраните текущий стек ниже",
        "None yet — save the current stack below",
    ),
    // Materials
    ("Сплошной", "Solid"),
    ("Акрил", "Acrylic"),
    ("Слюда", "Mica"),
    ("Стекломорфизм", "Glassmorphism"),
    ("Стекло", "Glass"),
    ("Классическое стекло", "Legacy Glass"),
    ("Дымка", "Smoke"),
    // Misc
    ("90° против часовой", "90° CCW"),
    ("90° по часовой", "90° CW"),
    ("Дизер (антибэндинг)", "Dither (антибэндинг)"),
    ("Обратить", "Reverse"),
    ("ФГ", "FG"),
    ("БГ", "BG"),
];

struct AddonLang {
    name: String,
    map: HashMap<String, String>,
}

struct Catalog {
    code: String,
    addons: HashMap<String, AddonLang>,
}

fn catalog() -> &'static Mutex<Catalog> {
    static C: OnceLock<Mutex<Catalog>> = OnceLock::new();
    C.get_or_init(|| {
        Mutex::new(Catalog {
            code: "ru".into(),
            addons: HashMap::new(),
        })
    })
}

fn ru_to_en() -> &'static HashMap<&'static str, &'static str> {
    static M: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    M.get_or_init(|| PAIRS.iter().copied().collect())
}

fn en_to_ru() -> &'static HashMap<&'static str, &'static str> {
    static M: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    M.get_or_init(|| PAIRS.iter().map(|(ru, en)| (*en, *ru)).collect())
}

fn builtin_lookup(code: &str, s: &str) -> Option<&'static str> {
    match code {
        "en" => ru_to_en().get(s).copied(),
        "ru" => en_to_ru().get(s).copied(),
        _ => None,
    }
}

pub fn set_language(code: &str) {
    let code = code.trim().to_ascii_lowercase();
    if code.is_empty() {
        return;
    }
    if let Ok(mut c) = catalog().lock() {
        c.code = code;
    }
}

pub fn current_code() -> String {
    catalog()
        .lock()
        .map(|c| c.code.clone())
        .unwrap_or_else(|_| "ru".into())
}

pub fn builtin_languages() -> &'static [(&'static str, &'static str)] {
    &[("ru", "Русский"), ("en", "English")]
}

pub fn addon_languages() -> Vec<(String, String)> {
    catalog()
        .lock()
        .map(|c| {
            c.addons
                .iter()
                .map(|(code, pack)| (code.clone(), pack.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn register_language(code: &str, name: &str) {
    let code = code.trim().to_ascii_lowercase();
    if code.is_empty() || code == "ru" || code == "en" {
        return;
    }
    if let Ok(mut c) = catalog().lock() {
        c.addons.entry(code).or_insert_with(|| AddonLang {
            name: name.trim().chars().take(64).collect(),
            map: HashMap::new(),
        });
    }
}

pub fn set_translation(code: &str, from: &str, to: &str) {
    let code = code.trim().to_ascii_lowercase();
    if code.is_empty() || from.is_empty() {
        return;
    }
    if let Ok(mut c) = catalog().lock() {
        let pack = c.addons.entry(code.clone()).or_insert_with(|| AddonLang {
            name: code.clone(),
            map: HashMap::new(),
        });
        pack.map.insert(
            from.chars().take(256).collect(),
            to.chars().take(256).collect(),
        );
    }
}

pub fn t(text: impl Into<String>) -> String {
    let s = text.into();
    if s.is_empty() {
        return s;
    }
    let code = current_code();
    if code == "ru" || code == "en" {
        if let Some(hit) = builtin_lookup(&code, &s) {
            return hit.to_string();
        }
        return s;
    }
    if let Ok(c) = catalog().lock() {
        if let Some(pack) = c.addons.get(&code) {
            if let Some(hit) = pack.map.get(&s) {
                return hit.clone();
            }
        }
    }
    // Addon lang missing a key → English, then original.
    if let Some(en) = builtin_lookup("en", &s) {
        return en.to_string();
    }
    s
}
