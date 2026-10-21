package dev.gitblame.voxgolem.capture

import android.app.Activity
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class CapturePlaybackListenerTest {
    @Test fun snakeCaseCommandsUseInheritedRegistrationAndRemoveTheExactRealChannel() {
        val activity = Robolectric.buildActivity(Activity::class.java).setup().get()
        val plugin = CapturePlugin(activity)
        val responses = mutableListOf<Long>()
        val messages = mutableListOf<Pair<Long, String>>()
        // Use Tauri's actual Channel deserializer/serializer; replace only its final transport.
        // No JNI dispatch or WebView delivery is asserted by this host test.
        val mapperClass = Class.forName("com.fasterxml.jackson.databind.ObjectMapper")
        val mapper = mapperClass.getConstructor().newInstance()
        val deserializerClass = Class.forName("app.tauri.plugin.ChannelDeserializer")
        val constructor = deserializerClass.declaredConstructors.single().apply { isAccessible = true }
        val send: (Long, String) -> Unit = { channel, value -> messages += channel to value }
        val deserializer = constructor.newInstance(send, mapper)
        val moduleClass = Class.forName("com.fasterxml.jackson.databind.module.SimpleModule")
        val module = moduleClass.getConstructor().newInstance()
        moduleClass.getMethod("addDeserializer", Class::class.java,
            Class.forName("com.fasterxml.jackson.databind.JsonDeserializer"))
            .invoke(module, Class.forName("app.tauri.plugin.Channel"), deserializer)
        mapperClass.getMethod("registerModule", Class.forName("com.fasterxml.jackson.databind.Module")).invoke(mapper, module)
        fun invoke(command: String, args: String): Invoke {
            val respond: (Long, String) -> Unit = { callback, _ -> responses += callback }
            return Invoke::class.java.constructors.single().newInstance(1L, command, 10L, 11L, respond, args, mapper) as Invoke
        }
        plugin.register_listener(invoke("register_listener", """{"event":"playback","handler":"__CHANNEL__:41"}"""))
        plugin.register_listener(invoke("register_listener", """{"event":"playback","handler":"__CHANNEL__:42"}"""))
        assertTrue(plugin.hasListener("playback"))
        plugin.trigger("playback", JSObject().put("phase", "preparing"))
        assertEquals(listOf(41L, 42L), messages.map { it.first })
        messages.clear()
        plugin.remove_listener(invoke("remove_listener", """{"event":"playback","channelId":41}"""))
        plugin.trigger("playback", JSObject().put("phase", "completed"))
        assertEquals(listOf(42L), messages.map { it.first })
        plugin.remove_listener(invoke("remove_listener", """{"event":"playback","channelId":42}"""))
        assertFalse(plugin.hasListener("playback"))
        assertEquals(listOf(10L, 10L, 10L, 10L), responses)
        plugin.onDestroy()
    }
}
