package dev.gitblame.voxgolem.android.wear.transport

import android.os.ParcelFileDescriptor
import com.google.android.gms.tasks.TaskCompletionSource
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.DataClient
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.InputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class ReceiptAssetReaderTest {
    @Test fun acquisitionTimeoutReleasesTheAssetDeliveredAfterTheReaderRetiresExactlyOnce() {
        val task = TaskCompletionSource<DataClient.GetFdForAssetResponse>()
        val reader = ReceiptAssetReader(task.task, 20)
        val worker = Thread { runCatching { reader.open() } }
        worker.start(); worker.join(2_000)
        assertFalse(worker.isAlive)
        val released = CountDownLatch(1)
        val releases = AtomicInteger()
        task.setResult(object : DataClient.GetFdForAssetResponse() {
            override fun getFdForAsset(): ParcelFileDescriptor = error("Synthetic stream-only asset")
            override fun getInputStream(): InputStream = error("Retired acquisition must not request a stream")
            override fun release() { releases.incrementAndGet(); released.countDown() }
        })
        assertTrue(released.await(2, TimeUnit.SECONDS))
        reader.close(); assertEquals(1, releases.get())
    }
    @Test fun blockedReceiptReadIsReleasedAtTheOwnedDeadlineAndCannotReturnTruncatedSuccess() {
        val reading = CountDownLatch(1)
        val unblock = CountDownLatch(1)
        val releases = AtomicInteger()
        val response = object : DataClient.GetFdForAssetResponse() {
            override fun getFdForAsset(): ParcelFileDescriptor = error("Synthetic stream-only asset")
            override fun getInputStream() = object : InputStream() {
                override fun read(): Int { reading.countDown(); check(unblock.await(5, TimeUnit.SECONDS)); return -1 }
            }
            override fun release() { releases.incrementAndGet(); unblock.countDown() }
        }
        val reader = ReceiptAssetReader(Tasks.forResult(response), 100)
        val failed = CountDownLatch(1)
        val worker = Thread { runCatching { reader.open().use { it.read() } }.onFailure { failed.countDown() } }
        worker.start(); assertTrue(reading.await(2, TimeUnit.SECONDS)); worker.join(2_000)
        assertFalse(worker.isAlive)
        assertEquals(0L, failed.count)
        assertEquals(1, releases.get())
        reader.close(); assertEquals(1, releases.get())
    }
}
