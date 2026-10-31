//! Category icons: one Lucide glyph per spending category, used wherever a
//! category is shown (Spending's bars, the transactions table, the detail
//! sheet, the palette, Budgets, Year in review, Travel's legend).
//!
//! Matching is by keyword, case-insensitive, on word starts ("rent" matches
//! "Rent" and "Rental", not "Current"), so the finer categories the rules
//! will grow into ("Dining: coffee", "Transport / taxi") still land well.
//! The first rule that matches wins; unknown names get a plain tag.
//!
//! Only the icons below are embedded (`icon_assets!`), not the whole catalog.

use gpui_kit::assets::{IconName as Lucide, icon_assets};
use gpui_kit::component::{ActiveTheme as _, Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::ui::Figures as _;
use crate::zoom::px;

/// The embedded icons, and [`NAMED`]: the ones a category can be given by
/// name (the categories table's `icon`, the Categories manager's picker).
macro_rules! category_icons {
    ([$($icon:ident),* $(,)?], [$($page:ident),* $(,)?]) => {
        icon_assets!(pub CategoryIcons, [$($icon,)* $($page,)*]);
        /// (Lucide name, icon) of every category icon, in the picker's order.
        pub const NAMED: &[(&str, Lucide)] = &[$((stringify!($icon), Lucide::$icon)),*];
    };
}

category_icons!([
    Utensils, Coffee, Beer, Wine, ShoppingCart, TramFront, Car, Fuel, TrainFront, Bus, Bike, Plane, BedDouble,
    Luggage, House, Plug, Zap, Droplet, Flame, Smartphone, Wifi, Repeat, Repeat2, Tv, Cloud, HeartPulse,
    Stethoscope, Pill, Shield, Dumbbell, Banknote, TrendingUp, Bitcoin, ChartCandlestick, Receipt,
    ShoppingBag, Shirt, Laptop, Package, Clapperboard, Music, Gamepad2, Ticket, Sofa, Wrench, Sparkles,
    Scissors, GraduationCap, BookOpen, Wallet, ArrowLeftRight, CircleDashed, Tag, Briefcase, Percent,
    HandCoins, Undo2, CreditCard, Coins, Landmark, Gift, HandHeart, Users, Heart, Baby, PawPrint,
    PiggyBank, ArrowDownLeft, ArrowUpRight,
], [
    // pages
    Target, CalendarRange, ListChecks, Lightbulb, Tags, Wand, Pencil, ChartGantt,
]);

/// The icon called `name` ("Utensils"), when it is one of [`NAMED`].
pub fn by_name(name: &str) -> Option<Lucide> {
    NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, i)| *i)
}

/// (keyword, icon). A keyword matches the start of a word of the category;
/// a short one (3 letters or less) the whole word or its plural ("bus" is not
/// "business"); one with a space or dash, anywhere. Order matters: specific
/// before general.
const RULES: &[(&str, Lucide)] = &[
    // money that isn't spending
    ("reimburs", Lucide::Briefcase), ("office", Lucide::Briefcase),
    ("salary", Lucide::Banknote), ("payroll", Lucide::Banknote), ("wage", Lucide::Banknote), ("bonus", Lucide::Banknote),
    ("income", Lucide::Banknote),
    ("interest", Lucide::Percent), ("rebate", Lucide::Percent), ("cashback", Lucide::Percent),
    ("dividend", Lucide::HandCoins), ("refund", Lucide::Undo2),
    ("transfer", Lucide::ArrowLeftRight), ("credit-card", Lucide::CreditCard), ("card bill", Lucide::CreditCard),
    ("fx", Lucide::Coins), ("currency", Lucide::Coins), ("exchange", Lucide::Coins),
    ("deposit", Lucide::PiggyBank), ("saving", Lucide::PiggyBank),
    ("invest", Lucide::TrendingUp), ("crypto", Lucide::Bitcoin), ("trade", Lucide::ChartCandlestick),
    ("reward", Lucide::Gift), ("buy", Lucide::ArrowDownLeft), ("convert", Lucide::Repeat2),
    ("sell", Lucide::ArrowUpRight), ("withdraw", Lucide::ArrowUpRight),
    // obligations
    ("student loan", Lucide::GraduationCap), ("loan", Lucide::Landmark), ("mortgage", Lucide::House),
    ("tax", Lucide::Landmark), ("government", Lucide::Landmark),
    ("insurance", Lucide::Shield),
    // getting around
    ("taxi", Lucide::Car), ("ride", Lucide::Car), ("car", Lucide::Car), ("parking", Lucide::Car),
    ("fuel", Lucide::Fuel), ("petrol", Lucide::Fuel), ("train", Lucide::TrainFront), ("rail", Lucide::TrainFront),
    ("bus", Lucide::Bus), ("bike", Lucide::Bike), ("transport", Lucide::TramFront), ("commut", Lucide::TramFront),
    ("travel", Lucide::Plane), ("flight", Lucide::Plane), ("airline", Lucide::Plane),
    ("hotel", Lucide::BedDouble), ("lodging", Lucide::BedDouble), ("accommodation", Lucide::BedDouble),
    ("holiday", Lucide::Luggage), ("vacation", Lucide::Luggage),
    // home
    ("rent", Lucide::House), ("housing", Lucide::House), ("home", Lucide::House),
    ("utilit", Lucide::Plug), ("electric", Lucide::Zap), ("power", Lucide::Zap), ("water", Lucide::Droplet),
    ("gas", Lucide::Flame), ("telecom", Lucide::Smartphone), ("phone", Lucide::Smartphone), ("mobile", Lucide::Smartphone),
    ("internet", Lucide::Wifi), ("broadband", Lucide::Wifi),
    ("household", Lucide::Sofa), ("furniture", Lucide::Sofa), ("repair", Lucide::Wrench), ("hardware", Lucide::Wrench),
    // regular life
    ("subscription", Lucide::Repeat), ("streaming", Lucide::Tv), ("software", Lucide::Cloud),
    ("health", Lucide::HeartPulse), ("medical", Lucide::Stethoscope), ("doctor", Lucide::Stethoscope),
    ("dental", Lucide::Stethoscope), ("pharma", Lucide::Pill),
    ("gym", Lucide::Dumbbell), ("fitness", Lucide::Dumbbell), ("sport", Lucide::Dumbbell),
    ("coffee", Lucide::Coffee), ("cafe", Lucide::Coffee), ("café", Lucide::Coffee),
    ("bar", Lucide::Beer), ("drink", Lucide::Wine), ("alcohol", Lucide::Wine),
    ("dining", Lucide::Utensils), ("restaurant", Lucide::Utensils), ("food", Lucide::Utensils),
    ("takeaway", Lucide::Utensils), ("eating", Lucide::Utensils), ("lunch", Lucide::Utensils),
    ("grocer", Lucide::ShoppingCart), ("supermarket", Lucide::ShoppingCart),
    // things and fun
    ("clothing", Lucide::Shirt), ("clothes", Lucide::Shirt), ("fashion", Lucide::Shirt), ("apparel", Lucide::Shirt),
    ("electronic", Lucide::Laptop), ("tech", Lucide::Laptop), ("online", Lucide::Package),
    ("shopping", Lucide::ShoppingBag), ("retail", Lucide::ShoppingBag),
    ("entertain", Lucide::Clapperboard), ("cinema", Lucide::Clapperboard), ("movie", Lucide::Clapperboard),
    ("music", Lucide::Music), ("game", Lucide::Gamepad2), ("gaming", Lucide::Gamepad2),
    ("concert", Lucide::Ticket), ("event", Lucide::Ticket), ("ticket", Lucide::Ticket),
    ("personal care", Lucide::Sparkles), ("beauty", Lucide::Sparkles), ("cosmetic", Lucide::Sparkles),
    ("hair", Lucide::Scissors), ("barber", Lucide::Scissors),
    ("education", Lucide::GraduationCap), ("school", Lucide::GraduationCap), ("tuition", Lucide::GraduationCap),
    ("course", Lucide::BookOpen), ("book", Lucide::BookOpen),
    ("cash", Lucide::Wallet), ("atm", Lucide::Wallet),
    ("fee", Lucide::Receipt), ("charge", Lucide::Receipt), ("commission", Lucide::Receipt),
    ("gift", Lucide::Gift), ("donation", Lucide::HandHeart), ("charity", Lucide::HandHeart),
    ("family", Lucide::Users), ("partner", Lucide::Heart), ("kid", Lucide::Baby), ("child", Lucide::Baby),
    ("baby", Lucide::Baby), ("pet", Lucide::PawPrint),
    ("other", Lucide::CircleDashed), ("uncategor", Lucide::CircleDashed), ("misc", Lucide::CircleDashed),
];

/// The icon for a category: the one the owner picked (the categories
/// table), else one matched from the name. Unknown names get a tag; an
/// empty one, "Other".
pub fn icon(category: &str) -> Lucide {
    if let Some(i) = finance_core::categories::icon(category).as_deref().and_then(by_name) {
        return i;
    }
    from_name(category)
}

/// The icon the name alone suggests (what a category without a pick shows).
pub fn from_name(category: &str) -> Lucide {
    let lower = category.to_lowercase();
    if lower.trim().is_empty() {
        return Lucide::CircleDashed;
    }
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    RULES.iter()
        .find(|(k, _)| {
            if k.contains([' ', '-']) {
                lower.contains(k)
            } else if k.chars().count() <= 3 {
                words.iter().any(|w| *w == *k || w.strip_suffix('s') == Some(k))
            } else {
                words.iter().any(|w| w.starts_with(k))
            }
        })
        .map(|(_, i)| *i)
        .unwrap_or(Lucide::Tag)
}

/// The bare glyph, `size` px, in `color`.
pub fn glyph(category: &str, size: f32, color: Hsla) -> Icon {
    Icon::new(icon(category)).size(px(size)).text_color(color)
}

/// Icon + name, muted, for table cells and captions.
pub fn label(category: &str, cx: &App) -> Div {
    label_with(category, category, cx)
}

/// The category's icon before any text (a caption that says more than the name).
pub fn label_with(category: &str, text: impl Into<SharedString>, cx: &App) -> Div {
    let t = cx.theme();
    h_flex()
        .gap(px(6.))
        .min_w_0()
        .items_center()
        .text_color(t.muted_foreground)
        .child(glyph(category, 13., t.muted_foreground.opacity(0.85)))
        .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(text.into()))
}

/// The icon in a soft rounded tile, `size` px square (list rows).
pub fn tile(category: &str, size: f32, color: Hsla) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded(px(size * 0.3))
        .bg(color.opacity(0.14))
        .flex()
        .items_center()
        .justify_center()
        .child(glyph(category, size * 0.56, color))
}

/// A legend entry whose marker is the category's icon in the series color.
pub fn legend(color: Hsla, category: &str, value: impl Into<SharedString>, cx: &App) -> Div {
    let value = value.into();
    let t = cx.theme();
    h_flex()
        .gap(px(5.))
        .text_size(px(crate::ui::ts::CAPTION))
        .whitespace_nowrap()
        .child(glyph(category, 12., color))
        .child(div().text_color(t.foreground).child(category.to_string()))
        .when(!value.is_empty(), |d| d.child(div().text_color(t.muted_foreground).figures().child(value)))
}

#[cfg(test)]
mod tests {
    // not super::*: no gpui `test` attribute in scope
    use super::{icon, Lucide};

    #[test]
    fn today_s_categories() {
        let cases = [
            ("Dining", Lucide::Utensils), ("Groceries", Lucide::ShoppingCart), ("Transport", Lucide::TramFront),
            ("Travel", Lucide::Plane), ("Rent", Lucide::House), ("Utilities / Telecom", Lucide::Plug),
            ("Subscriptions", Lucide::Repeat), ("Health", Lucide::HeartPulse), ("Gym", Lucide::Dumbbell),
            ("Salary", Lucide::Banknote), ("Investments", Lucide::TrendingUp), ("Fees", Lucide::Receipt),
            ("Shopping", Lucide::ShoppingBag), ("Entertainment", Lucide::Clapperboard), ("Household", Lucide::Sofa),
            ("Personal care", Lucide::Sparkles), ("Education", Lucide::GraduationCap), ("Cash / ATM", Lucide::Wallet),
            ("Internal transfer", Lucide::ArrowLeftRight), ("Transfer: self/exchange", Lucide::ArrowLeftRight),
            ("Other", Lucide::CircleDashed), ("Interest / rebates", Lucide::Percent),
            ("Credit-card bill", Lucide::CreditCard), ("FX exchange", Lucide::Coins),
            ("Office (reimbursed)", Lucide::Briefcase), ("Student loan", Lucide::GraduationCap),
            ("Partner", Lucide::Heart), ("Deposit", Lucide::PiggyBank), ("Family", Lucide::Users),
            ("reward", Lucide::Gift), ("auto-invest", Lucide::TrendingUp), ("spot-buy", Lucide::ArrowDownLeft),
        ];
        for (c, want) in cases {
            assert_eq!(icon(c), want, "{c}");
        }
    }

    #[test]
    fn finer_and_unknown_categories() {
        assert_eq!(icon("DINING: coffee"), Lucide::Coffee);
        assert_eq!(icon("Transport / Taxi"), Lucide::Car);
        assert_eq!(icon("Current account"), Lucide::Tag, "'rent' only at a word start");
        assert_eq!(icon("Rental car"), Lucide::Car, "the more specific rule");
        assert_eq!(icon("Pets"), Lucide::PawPrint);
        assert_eq!(icon("Business"), Lucide::Tag, "not a bus");
        assert_eq!(icon("Weird stuff"), Lucide::Tag);
        assert_eq!(icon(""), Lucide::CircleDashed);
    }
}
