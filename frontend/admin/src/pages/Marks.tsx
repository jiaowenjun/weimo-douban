import { useCallback, useEffect, useState } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';
import {
  Alert,
  App as AntApp,
  Avatar,
  Button,
  Card,
  Form,
  Rate,
  Select,
  Space,
  Table,
  Tag,
  theme,
  Tooltip,
  Typography,
} from 'antd';
import { PageContainer } from '@ant-design/pro-components';
import { SearchOutlined } from '@ant-design/icons';
import dayjs from 'dayjs';
import type { MarksResponse, RecordItem } from '../types';
import { api, resolveApiUrl, type ApiError } from '../api';
import { CATEGORY_OPTIONS, STATUS_OPTIONS, dimLabel } from '../labels';
import { UserAvatar, useUserProfile } from '../user-profile';
import { SubjectMetaDetail, SubjectNotEnriched } from '../subject-meta';

const PAGE_SIZE = 20;

interface Query {
  uid: string;
  category: string;
  status: string;
}

export default function Marks() {
  const navigate = useNavigate();
  const { message } = AntApp.useApp();
  const { token } = theme.useToken();
  const [form] = Form.useForm<{ uid: string; category: string; status: string }>();
  const [query, setQuery] = useState<Query | null>(null);
  const [page, setPage] = useState(1);
  const [data, setData] = useState<MarksResponse | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [loading, setLoading] = useState(false);
  const [noUsers, setNoUsers] = useState(false);
  const [userOptions, setUserOptions] = useState<{ value: string; label: string }[]>([]);
  const [searchParams] = useSearchParams();
  const headerProfile = useUserProfile(data?.user ?? null);

  // 挂载或深链变化时确定目标用户：/marks?uid= 深链优先；无深链时自动选中
  // 第一个用户（后端按记录数降序）直接进入浏览；库内完全无用户时提示先建任务
  useEffect(() => {
    const uidParam = searchParams.get('uid');
    const category = searchParams.get('category') ?? 'movie';
    const status = searchParams.get('status') ?? 'collect';
    if (uidParam) {
      form.setFieldsValue({ uid: uidParam, category, status });
      setPage(1);
      setQuery({ uid: uidParam, category, status });
    }
    let alive = true;
    api
      .listUsers()
      .then((res) => {
        if (!alive) return;
        const options = res.users.map((u) => ({
          value: u.uid,
          label: u.username ? `${u.username}（@${u.uid}）` : `@${u.uid}`,
        }));
        setUserOptions(options);
        if (!uidParam) {
          if (options.length > 0) {
            form.setFieldsValue({ uid: options[0].value, category, status });
            setPage(1);
            setQuery({ uid: options[0].value, category, status });
          } else {
            setNoUsers(true);
          }
        }
      })
      .catch(() => {
        if (alive && !uidParam) setNoUsers(true);
      });
    return () => {
      alive = false;
    };
  }, [searchParams, form]);

  const load = useCallback(async () => {
    if (!query) return;
    setLoading(true);
    try {
      setData(await api.getMarks(query.uid, query.category, query.status, page, PAGE_SIZE, { expandSubject: true }));
      setError(null);
    } catch (e) {
      setData(null);
      setError(e as ApiError);
    } finally {
      setLoading(false);
    }
  }, [query, page]);

  useEffect(() => {
    load();
  }, [load]);

  const onSearch = (v: { uid: string; category: string; status: string }) => {
    if (!v.uid) {
      message.warning('请先选择要浏览的用户');
      return;
    }
    setPage(1);
    setQuery({ uid: v.uid, category: v.category, status: v.status });
  };

  return (
    <PageContainer title="数据浏览" subTitle="直接读取服务器缓存的书影音标记数据，不触发抓取">
      {noUsers && (
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 16 }}
          message="库内暂无用户数据"
          description="先在「用户」页或「同步任务」页为某个豆瓣用户创建同步任务，完成后即可在此浏览。"
        />
      )}
      <Card size="small" style={{ marginBottom: 16 }}>
        <Form form={form} layout="inline" onFinish={onSearch} initialValues={{ category: 'movie', status: 'collect' }}>
          <Form.Item name="uid" rules={[{ required: true, message: '请选择用户' }]}>
            <Select
              showSearch
              options={userOptions}
              style={{ width: 220 }}
              placeholder="选择用户"
              optionFilterProp="label"
            />
          </Form.Item>
          <Form.Item name="category">
            <Select options={CATEGORY_OPTIONS} style={{ width: 100 }} />
          </Form.Item>
          <Form.Item name="status">
            <Select options={STATUS_OPTIONS} style={{ width: 180 }} />
          </Form.Item>
          <Form.Item>
            <Button type="primary" htmlType="submit" icon={<SearchOutlined />}>
              查询
            </Button>
          </Form.Item>
        </Form>
      </Card>

      {error && (
        <Alert
          type={error.code === 'DIMENSION_NOT_CACHED' ? 'info' : 'error'}
          showIcon
          style={{ marginBottom: 16 }}
          message={error.message}
          description={
            error.code === 'DIMENSION_NOT_CACHED'
              ? '该维度暂无缓存数据，请先创建同步任务，完成后即可在此浏览。'
              : undefined
          }
          action={
            error.code === 'DIMENSION_NOT_CACHED' ? (
              <Button size="small" type="primary" onClick={() => navigate('/tasks')}>
                去创建任务
              </Button>
            ) : undefined
          }
        />
      )}

      {data && (
        <div style={{ marginBottom: 12 }}>
          <Space size={12}>
            <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
              <UserAvatar uid={data.user} size={20} />
              {headerProfile?.username || data.user} 的 {dimLabel(data.category, data.status)} · 共{' '}
              {data.total} 条
            </span>
            <Tag color={data.fresh ? 'success' : 'default'}>{data.fresh ? '缓存有效' : '缓存已过期'}</Tag>
            <span style={{ color: token.colorTextDescription }}>抓取于 {dayjs(data.cached_at).format('YYYY-MM-DD HH:mm')}</span>
          </Space>
        </div>
      )}

      <Table<RecordItem>
        size="small"
        rowKey={(r) => r.comment_id || r.subject_id}
        loading={loading}
        dataSource={data?.items ?? []}
        expandable={{
          expandedRowRender: (r) =>
            r.subject && r.subject.source !== 'intro' ? (
              <SubjectMetaDetail subject={r.subject} />
            ) : (
              <>
                {r.subject && <SubjectMetaDetail subject={r.subject} />}
                <SubjectNotEnriched category={r.category} subjectId={r.subject_id} fromIntro />
              </>
            ),
        }}
        pagination={
          data
            ? {
                current: page,
                pageSize: PAGE_SIZE,
                total: data.total,
                showSizeChanger: false,
                onChange: setPage,
              }
            : false
        }
        columns={[
          {
            title: '',
            dataIndex: 'cover',
            width: 56,
            // 无封面（空串）传 undefined 让 antd Avatar 直接渲染默认图标——
            // 空串 src 部分浏览器不发 error 事件、不触发图标回退
            render: (v) => <Avatar shape="square" size={44} src={v ? resolveApiUrl(v) : undefined} />,
          },
          {
            title: '条目',
            dataIndex: 'title',
            render: (_, r) => (
              <>
                <div>
                  <a href={r.subject_url} target="_blank" rel="noreferrer">
                    {r.title}
                  </a>
                </div>
                {r.alt_title && (
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    {r.alt_title}
                  </Typography.Text>
                )}
              </>
            ),
          },
          {
            title: '评分',
            dataIndex: 'my_rating',
            width: 130,
            render: (v) => (v ? <Rate disabled value={v} allowHalf style={{ fontSize: 14 }} /> : '—'),
          },
          {
            title: '豆瓣评分',
            width: 90,
            render: (_, r) =>
              r.subject?.rating.value != null ? (
                <span>
                  <span style={{ color: '#faad14', fontWeight: 600 }}>{r.subject.rating.value.toFixed(1)}</span>
                  <span style={{ color: token.colorTextTertiary, fontSize: 12 }}> / 10</span>
                </span>
              ) : (
                <span style={{ color: token.colorTextQuaternary }}>—</span>
              ),
          },
          { title: '日期', dataIndex: 'date', width: 100 },
          {
            title: '短评',
            dataIndex: 'comment',
            ellipsis: { showTitle: false },
            render: (v) =>
              v ? (
                <Tooltip title={v} placement="topLeft">
                  {v}
                </Tooltip>
              ) : (
                <span style={{ color: token.colorTextQuaternary }}>—</span>
              ),
          },
        ]}
      />
    </PageContainer>
  );
}
