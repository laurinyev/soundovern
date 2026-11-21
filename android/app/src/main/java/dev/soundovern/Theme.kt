package dev.soundovern

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/** Warm dark palette: charcoal surfaces, cream text, clay-orange accent. */
object Palette {
    val Bg = Color(0xFF1F1E1D)
    val Surface = Color(0xFF2B2A28)
    val SurfaceHi = Color(0xFF363532)
    val Outline = Color(0xFF4B4944)
    val Text = Color(0xFFF5F4EE)
    val Muted = Color(0xFFA9A69B)
    val Clay = Color(0xFFD97757)
    val ClayDim = Color(0xFF8A4A36)
    val Ok = Color(0xFF9CC28A)
    val Warn = Color(0xFFE0B15C)
    val Err = Color(0xFFE57B6E)
}

private val Scheme = darkColorScheme(
    primary = Palette.Clay,
    onPrimary = Palette.Bg,
    secondary = Palette.Clay,
    background = Palette.Bg,
    onBackground = Palette.Text,
    surface = Palette.Surface,
    onSurface = Palette.Text,
    surfaceVariant = Palette.SurfaceHi,
    onSurfaceVariant = Palette.Muted,
    outline = Palette.Outline,
    error = Palette.Err,
)

private val Sans = FontFamily.SansSerif
private val Serif = FontFamily.Serif

private val Type = Typography(
    headlineLarge = TextStyle(fontFamily = Serif, fontWeight = FontWeight.Medium, fontSize = 34.sp, lineHeight = 40.sp, letterSpacing = (-0.5).sp),
    headlineMedium = TextStyle(fontFamily = Serif, fontWeight = FontWeight.Medium, fontSize = 26.sp, lineHeight = 32.sp),
    titleMedium = TextStyle(fontFamily = Sans, fontWeight = FontWeight.SemiBold, fontSize = 16.sp, lineHeight = 22.sp),
    bodyMedium = TextStyle(fontFamily = Sans, fontSize = 14.sp, lineHeight = 21.sp),
    bodySmall = TextStyle(fontFamily = Sans, fontSize = 12.sp, lineHeight = 17.sp),
    labelMedium = TextStyle(fontFamily = Sans, fontWeight = FontWeight.Medium, fontSize = 12.sp, letterSpacing = 0.4.sp),
    labelLarge = TextStyle(fontFamily = Sans, fontWeight = FontWeight.SemiBold, fontSize = 16.sp),
)

@Composable
fun SoundovernTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = Scheme, typography = Type, content = content)
}
