import { Alert, Button, Space, Tag, Typography } from 'antd';
import { useEffect, useState } from 'react';
import { api, resolveApiUrl, type ApiError } from './api';
import type { Subject } from './types';

/** 豆瓣公共评分（区别于用户自己的 my_rating） */
export function SubjectRating({ subject }: { subject: Subject }) {
  const { value, count } = subject.rating;
  if (value == null) return <span style={{ opacity: 0.4 }}>—</span>;
  const fmtCount =
    count == null ? '' : count >= 10000 ? `${(count / 10000).toFixed(1)}万人评价` : `${count}人评价`;
  return (
    <Space size={4}>
      <Typography.Text strong style={{ color: '#faad14' }}>{value.toFixed(1)}</Typography.Text>
      <Typography.Text type="secondary" style={{ fontSize: 12 }}>{fmtCount}</Typography.Text>
    </Space>
  );
}

/** 行内一行式摘要：电影=导演/主演/类型；书=作者/出版社；音乐=歌手/流派/介质 */
export function SubjectKeyFields({ subject }: { subject: Subject }) {
  const join = (arr?: string[], max = 3) => {
    const list = (arr ?? []).filter(Boolean).slice(0, max).join(' / ');
    return list || null;
  };
  const parts: (string | null | undefined)[] =
    subject.category === 'movie'
      ? [
          join(subject.movie?.directors, 2) && `导演：${join(subject.movie?.directors, 2)}`,
          join(subject.movie?.actors) && `主演：${join(subject.movie?.actors)}`,
          join(subject.movie?.genres),
          join(subject.movie?.countries),
          subject.movie?.durations?.[0],
        ]
      : subject.category === 'book'
        ? [
            join(subject.book?.authors, 3) && `作者：${join(subject.book?.authors, 3)}`,
            join(subject.book?.translators, 2) && `译者：${join(subject.book?.translators, 2)}`,
            join(subject.book?.press),
            subject.book?.pubdate?.[0],
          ]
        : [
            join(subject.music?.singers, 3) && `艺人：${join(subject.music?.singers, 3)}`,
            join(subject.music?.genres),
            join(subject.music?.media),
            subject.music?.pubdate?.[0],
          ];
  const chips = parts.filter(Boolean) as string[];
  return (
    <Typography.Text type="secondary" style={{ fontSize: 12 }}>
      {chips.join(' · ') || '—'}
    </Typography.Text>
  );
}

function Field({ label, value }: { label: string; value?: string | null }) {
  if (!value) return null;
  return (
    <Tag style={{ marginInlineEnd: 0 }}>
      <Typography.Text type="secondary" style={{ fontSize: 12 }}>{label} </Typography.Text>
      {value}
    </Tag>
  );
}

/** 行展开完整详情：全部结构化字段 + 剧情简介/内容简介 */
export function SubjectMetaDetail({ subject }: { subject: Subject }) {
  const field = (v?: string[] | null, max = 99) =>
    (v ?? []).filter(Boolean).slice(0, max).join(' / ') || null;
  return (
    <div style={{ padding: '4px 0' }}>
      <Space size={[8, 8]} wrap style={{ marginBottom: 8 }}>
        {subject.original_title && <Field label="原名" value={subject.original_title} />}
        {subject.aka.length > 0 && <Field label="又名" value={field(subject.aka)} />}
        <Field label="年份" value={subject.year?.toString()} />
        {subject.category === 'movie' && subject.movie && (
          <>
            <Field label="导演" value={field(subject.movie.directors)} />
            <Field label="主演" value={field(subject.movie.actors, 10)} />
            <Field label="类型" value={field(subject.movie.genres)} />
            <Field label="国家/地区" value={field(subject.movie.countries)} />
            <Field label="语言" value={field(subject.movie.languages)} />
            <Field label="片长" value={field(subject.movie.durations)} />
            <Field label="上映" value={field(subject.movie.pubdate)} />
          </>
        )}
        {subject.category === 'book' && subject.book && (
          <>
            <Field label="作者" value={field(subject.book.authors)} />
            <Field label="译者" value={field(subject.book.translators)} />
            <Field label="出版社" value={field(subject.book.press)} />
            <Field label="出版" value={field(subject.book.pubdate)} />
            <Field label="页数" value={field(subject.book.pages)} />
            <Field label="定价" value={field(subject.book.price)} />
            <Field label="丛书" value={subject.book.series} />
          </>
        )}
        {subject.category === 'music' && subject.music && (
          <>
            <Field label="艺人" value={field(subject.music.singers)} />
            <Field label="流派" value={field(subject.music.genres)} />
            <Field label="介质" value={field(subject.music.media)} />
            <Field label="碟片" value={field(subject.music.discs)} />
            <Field label="发行" value={field(subject.music.pubdate)} />
            <Field label="出版方" value={field(subject.music.publisher)} />
            {subject.music.songs.length > 0 && <Field label="曲目" value={field(subject.music.songs, 99)} />}
          </>
        )}
      </Space>
      {subject.movie?.intro || subject.book?.intro || subject.music?.intro ? (
        <Typography.Paragraph
          type="secondary"
          ellipsis={{ rows: 4, expandable: true, symbol: '展开' }}
          style={{ marginBottom: 0, fontSize: 12, maxWidth: 860 }}
        >
          {subject.movie?.intro ?? subject.book?.intro ?? subject.music?.intro}
        </Typography.Paragraph>
      ) : null}
    </div>
  );
}

/** marks 行展开的空态：条目尚未富集（或当前为 intro 启发式兜底），提供「立即富集」 */
export function SubjectNotEnriched({
  category,
  subjectId,
  fromIntro = false,
}: {
  category: string;
  subjectId: string;
  fromIntro?: boolean;
}) {
  const [loading, setLoading] = useState(false);
  const [queued, setQueued] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => () => setQueued(false), [subjectId]);
  return (
    <Alert
      type="info"
      showIcon
      style={{ padding: '4px 12px' }}
      message={
        queued ? (
          '已加入富集队列，worker 空闲时段抓取豆瓣（rexxar），稍后刷新本页查看'
        ) : (
          <>
            {fromIntro
              ? '以上为 intro 启发式解析结果（字段可能有误、无豆瓣评分），可富集为 rexxar 权威数据。'
              : '该条目尚未富集结构化元信息。'}
            <Button
              size="small"
              type="link"
              loading={loading}
              onClick={async () => {
                setLoading(true);
                setErr(null);
                try {
                  await api.refreshSubject(category, subjectId);
                  setQueued(true);
                } catch (e) {
                  setErr((e as ApiError).message);
                } finally {
                  setLoading(false);
                }
              }}
            >
              立即富集
            </Button>
            {err && <Typography.Text type="danger" style={{ fontSize: 12 }}>{err}</Typography.Text>}
          </>
        )
      }
    />
  );
}

/** 封面图（走本地 covers 代理，含 token 兜底）；无封面（豆瓣兜底图
 *  在后端归一后的空串）渲染中性占位块，避免 <img src=""> 裂图 */
export function SubjectCover({ subject, size = 44 }: { subject: Subject; size?: number }) {
  if (!subject.cover) {
    return (
      <div
        aria-label={`${subject.title} 无封面`}
        role="img"
        style={{ width: size, height: size * 1.4, borderRadius: 4, background: '#f5f5f5' }}
      />
    );
  }
  return (
    // eslint-disable-next-line @next/next/no-img-element
    <img
      src={resolveApiUrl(subject.cover)}
      alt={subject.title}
      width={size}
      height={size * 1.4}
      style={{ objectFit: 'cover', borderRadius: 4, display: 'block' }}
    />
  );
}
