/*
 * Copyright 2022 The Blaze Authors
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

package org.apache.spark.sql.execution.blaze.shuffle

import java.io.File
import java.io.FileInputStream
import java.io.FilterInputStream
import java.io.InputStream
import java.lang.reflect.Field
import java.lang.reflect.Method

import org.apache.spark.InterruptibleIterator
import org.apache.spark.MapOutputTracker
import org.apache.spark.SparkEnv
import org.apache.spark.TaskContext
import org.apache.spark.internal.Logging
import org.apache.spark.internal.config
import org.apache.spark.network.util.LimitedInputStream
import org.apache.spark.serializer.SerializerManager
import org.apache.spark.shuffle.BaseShuffleHandle
import org.apache.spark.shuffle.ShuffleReader
import org.apache.spark.shuffle.ShuffleReadMetricsReporter
import org.apache.spark.sql.execution.blaze.arrowio.ArrowReaderIterator
import org.apache.spark.sql.execution.blaze.arrowio.IpcInputStreamIterator
import org.apache.spark.storage.BlockId
import org.apache.spark.storage.BlockManager
import org.apache.spark.storage.FileSegment
import org.apache.spark.storage.ShuffleBlockFetcherIterator
import org.apache.spark.util.CompletionIterator

class ArrowBlockStoreShuffleReader[K, C](
    handle: BaseShuffleHandle[K, _, C],
    startPartition: Int,
    endPartition: Int,
    context: TaskContext,
    readMetrics: ShuffleReadMetricsReporter,
    serializerManager: SerializerManager = SparkEnv.get.serializerManager,
    blockManager: BlockManager = SparkEnv.get.blockManager,
    mapOutputTracker: MapOutputTracker = SparkEnv.get.mapOutputTracker,
    startMapId: Option[Int] = None,
    endMapId: Option[Int] = None,
    isLocalShuffleReader: Boolean = false)
    extends ShuffleReader[K, C]
    with Logging {

  private val dep = handle.dependency

  private def readBlocks(): Iterator[(BlockId, InputStream)] = {
    val start = System.currentTimeMillis()
    val blocksByAddress = (startMapId, endMapId) match {
      case (Some(startId), Some(endId)) =>
        mapOutputTracker.getMapSizesByExecutorId(
          handle.shuffleId,
          startPartition,
          endPartition,
          startId,
          endId,
          dep.serializer.supportsRelocationOfSerializedObjects)
      case (None, None) =>
        mapOutputTracker.getMapSizesByExecutorId(
          handle.shuffleId,
          startPartition,
          endPartition,
          dep.serializer.supportsRelocationOfSerializedObjects)
      case (_, _) =>
        throw new IllegalArgumentException("startMapId and endMapId should be both set or unset")
    }
    val end = System.currentTimeMillis()
    context.taskMetrics().executionPhaseMetric.incProduceFetchRequestTime(end - start)

    val blockStreams = new ShuffleBlockFetcherIterator(
      context,
      blockManager.shuffleClient,
      blockManager,
      blocksByAddress,
      streamWrapper = (_, in) => in,
      // Note: we use getSizeAsMb when no suffix is provided for backwards compatibility
      SparkEnv.get.conf.getSizeAsMb("spark.reducer.maxSizeInFlight", "48m") * 1024 * 1024,
      SparkEnv.get.conf.getInt("spark.reducer.maxReqsInFlight", Int.MaxValue),
      SparkEnv.get.conf.get(config.REDUCER_MAX_BLOCKS_IN_FLIGHT_PER_ADDRESS),
      SparkEnv.get.conf.get(config.MAX_REMOTE_BLOCK_SIZE_FETCH_TO_MEM),
      SparkEnv.get.conf.getBoolean("spark.shuffle.detectCorrupt", true),
      readMetrics,
      localShuffle = isLocalShuffleReader &&
        SparkEnv.get.conf.getBoolean("spark.kwai.localShuffle.readHdfs.enabled", false))

    blockStreams
  }

  /** Read the combined key-values for this reduce task */
  override def read(): Iterator[Product2[K, C]] = {
    val recordIter = readBlocks()
      .flatMap {
        case (_, inputStream) =>
          IpcInputStreamIterator(inputStream, decompressingNeeded = true, context)
      }
      .flatMap { channel =>
        new ArrowReaderIterator(channel, context).map((0, _)) // use 0 as key since it's not used
      }

    // Update the context task metrics for each record read.
    val metricIter = CompletionIterator[(Any, Any), Iterator[(Any, Any)]](
      recordIter.map { record =>
        readMetrics.incRecordsRead(1)
        record
      },
      context.taskMetrics().mergeShuffleReadMetrics(true))

    // An interruptible iterator must be used here in order to support task cancellation
    val interruptibleIter = new InterruptibleIterator[(Any, Any)](context, metricIter)

    val aggregatedIter: Iterator[Product2[K, C]] = if (dep.aggregator.isDefined) {
      throw new UnsupportedOperationException("aggregate not allowed")
    } else {
      interruptibleIter.asInstanceOf[Iterator[Product2[K, C]]]
    }

    // Sort the output if there is a sort ordering defined.
    val resultIter = dep.keyOrdering match {
      case Some(keyOrd: Ordering[K]) =>
        throw new UnsupportedOperationException("order not allowed")
      case None =>
        aggregatedIter
    }

    resultIter match {
      case _: InterruptibleIterator[Product2[K, C]] => resultIter
      case _ =>
        // Use another interruptible iterator here to support task cancellation as aggregator
        // or(and) sorter may have consumed previous interruptible iterator.
        new InterruptibleIterator[Product2[K, C]](context, resultIter)
    }
  }

  def readIpc(): Iterator[Object] = { // FileSegment | ReadableByteChannel
    val ipcIterator = readBlocks().flatMap {
      case (_, inputStream) =>
        ArrowBlockStoreShuffleReader.Helper.getFileSegmentFromInputStream(inputStream) match {
          case Some(fileSegment) =>
            Iterator.single(fileSegment)
          case None =>
            IpcInputStreamIterator(inputStream, decompressingNeeded = false, context)
        }
    }

    // An interruptible iterator must be used here in order to support task cancellation
    new InterruptibleIterator[Object](context, ipcIterator)
  }
}

object ArrowBlockStoreShuffleReader {
  private object Helper {
    val bufferReleasingInputStreamClass: Class[_] =
    // scalastyle:off classforname
      Class.forName("org.apache.spark.storage.BufferReleasingInputStream")
    // scalastyle:on classforname
    val delegateFn: Method = bufferReleasingInputStreamClass.getDeclaredMethod(
      "org$apache$spark$storage$BufferReleasingInputStream$$delegate")
    val inField: Field = classOf[FilterInputStream].getDeclaredField("in")
    val limitField: Field = classOf[LimitedInputStream].getDeclaredField("left")
    val pathField: Field = classOf[FileInputStream].getDeclaredField("path")
    delegateFn.setAccessible(true)
    inField.setAccessible(true)
    limitField.setAccessible(true)
    pathField.setAccessible(true)

    def getFileSegmentFromInputStream(in: InputStream): Option[FileSegment] = {
      delegateFn.invoke(in) match {
        case in: LimitedInputStream =>
          val limit = Helper.limitField.getLong(in)
          Helper.inField.get(in) match {
            case in: FileInputStream =>
              val path = Helper.pathField.get(in).asInstanceOf[String]
              val offset = in.getChannel.position()
              val fileSegment = new FileSegment(new File(path), offset, limit, 0)
              Some(fileSegment)
            case _ =>
              None
          }
        case _ =>
          None
      }
    }
  }
}